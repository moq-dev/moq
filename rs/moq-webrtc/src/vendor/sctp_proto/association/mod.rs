use crate::association::state::{AckMode, AckState, AssociationState};
use crate::association::stats::AssociationStats;
use crate::chunk::Chunk;
use crate::chunk::ErrorCauseUnrecognizedChunkType;
use crate::chunk::USER_INITIATED_ABORT;
use crate::chunk::chunk_abort::ChunkAbort;
use crate::chunk::chunk_cookie_ack::ChunkCookieAck;
use crate::chunk::chunk_cookie_echo::ChunkCookieEcho;
use crate::chunk::chunk_error::ChunkError;
use crate::chunk::chunk_forward_tsn::{ChunkForwardTsn, ChunkForwardTsnStream};
use crate::chunk::chunk_heartbeat::ChunkHeartbeat;
use crate::chunk::chunk_heartbeat_ack::ChunkHeartbeatAck;
use crate::chunk::chunk_i_forward_tsn::ChunkIForwardTsn;
use crate::chunk::chunk_init::{ChunkInit, ChunkInitAck};
use crate::chunk::chunk_payload_data::{ChunkPayloadData, PayloadProtocolIdentifier};
use crate::chunk::chunk_reconfig::ChunkReconfig;
use crate::chunk::chunk_selective_ack::ChunkSelectiveAck;
use crate::chunk::chunk_shutdown::ChunkShutdown;
use crate::chunk::chunk_shutdown_ack::ChunkShutdownAck;
use crate::chunk::chunk_shutdown_complete::ChunkShutdownComplete;
use crate::chunk::chunk_type::CT_FORWARD_TSN;
use crate::config::COMMON_HEADER_SIZE;
use crate::config::DATA_CHUNK_HEADER_SIZE;
use crate::config::{ServerConfig, TransportConfig};
use crate::error::{Error, Result};
use crate::packet::{CommonHeader, Packet};
use crate::param::Param;
use crate::param::param_heartbeat_info::ParamHeartbeatInfo;
use crate::param::param_outgoing_reset_request::ParamOutgoingResetRequest;
use crate::param::param_reconfig_response::{ParamReconfigResponse, ReconfigResult};
use crate::param::param_state_cookie::ParamStateCookie;
use crate::param::param_supported_extensions::ParamSupportedExtensions;
use crate::queue::outbound_queue::OutboundQueue;
use crate::queue::payload_queue::PayloadQueue;
use crate::queue::reassembly_queue::ReassemblyQueue;
use crate::shared::{AssociationEventInner, AssociationId, EndpointEvent, EndpointEventInner};
use crate::util::{sna16lt, sna32gt, sna32gte, sna32lt, sna32lte};
use crate::{AssociationEvent, Payload, Side, Transmit};
use stream::DataChannelStats;
use stream::{ReliabilityType, Stream, StreamEvent, StreamId, StreamResetError, StreamState};
use timer::{ACK_INTERVAL, RtoManager, Timer, TimerTable};

use crate::association::stream::RecvSendState;
use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use bytes::Bytes;
use core::net::{IpAddr, SocketAddr};
use core::num::NonZeroU32;
use core::str::FromStr;
use core::time::Duration;
use log::{debug, error, trace, warn};
use rand::random;
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::HashMap;
use std::time::Instant;
use thiserror::Error;

pub(crate) mod state;
pub(crate) mod stats;
pub(crate) mod stream;
mod timer;

#[cfg(test)]
mod association_test;

#[cfg(test)]
mod timed_reliability_test;

/// Reasons why an association might be lost
#[non_exhaustive]
#[derive(Debug, Error, Clone, PartialEq)]
pub enum AssociationError {
    /// Handshake failed
    #[error("{0}")]
    HandshakeFailed(#[from] Error),
    /// The peer violated the SCTP specification as understood by this implementation
    #[error("transport error")]
    TransportError,
    /// Receive storage filled with messages that the whole-message API cannot drain.
    #[error("receive buffer exhausted during message reassembly")]
    ReceiveBufferExhausted,
    /// The peer's SCTP stack aborted the association
    #[error("aborted by peer")]
    AssociationClosed,
    /// The peer closed the association
    #[error("closed by peer")]
    ApplicationClosed,
    /// The peer is unable to continue processing this association, usually due to having restarted
    #[error("reset by peer")]
    Reset,
    /// Communication with the peer has lapsed for longer than the negotiated idle timeout
    ///
    /// If neither side is sending keep-alives, an association will time out after a long enough idle
    /// period even if the peer is still reachable
    #[error("timed out")]
    TimedOut,
    /// The local application closed the association
    #[error("closed")]
    LocallyClosed,
}

/// Events of interest to the application
#[non_exhaustive]
#[derive(Debug)]
pub enum Event {
    /// The association was successfully established
    Connected,
    /// The association handshake failed
    ///
    /// Emitted if the handshake (INIT/COOKIE exchange) fails.
    HandshakeFailed {
        /// Reason that the handshake failed
        reason: AssociationError,
    },
    /// The association was lost
    ///
    /// Emitted if the peer closes the association or an error is encountered
    AssociationLost {
        /// Reason that the association was closed
        reason: AssociationError,
    },
    /// Stream events
    Stream(StreamEvent),
    /// Final counters for a removed stream generation, including association loss.
    ///
    /// Emitted exactly once per removed generation. Drain events after reads and
    /// close/reset operations as well as transport input, and before dropping
    /// the association. The embedding layer owns retention of closed reports.
    DataChannelStatsFinal(DataChannelStats),
    /// One or more application datagrams have been received
    DatagramReceived,
}

/// Multiset of stream ids owing a terminal reset event.
/// Each `Finished` for an id arms one result, a re-created id is a distinct
/// generation and owes its own.
#[derive(Debug, Default)]
struct PendingResetCompletions(FxHashMap<StreamId, usize>);

impl PendingResetCompletions {
    /// Arm one completion for this id.
    fn insert(&mut self, id: StreamId) {
        *self.0.entry(id).or_default() += 1;
    }

    fn contains(&self, id: &StreamId) -> bool {
        self.0.contains_key(id)
    }

    /// Consume one armed completion for this id.
    fn take_one(&mut self, id: StreamId) -> bool {
        if let Some(count) = self.0.get_mut(&id) {
            *count -= 1;
            if *count == 0 {
                self.0.remove(&id);
            }
            true
        } else {
            false
        }
    }

    fn clear(&mut self) {
        self.0.clear();
    }
}

#[derive(Debug, Copy, Clone)]
enum DeferredForwardTsnKind {
    Ordered {
        last_ssn: u16,
        new_cumulative_tsn: u32,
    },
    Unordered {
        new_cumulative_tsn: u32,
    },
}

#[derive(Debug, Copy, Clone)]
struct DeferredForwardTsn {
    /// The reset boundary ending the generation this update belongs to.
    /// None identifies the active tail generation after all accepted resets.
    generation_boundary: Option<u32>,
    kind: DeferredForwardTsnKind,
}

///Association represents an SCTP association
//13.2.  Parameters Necessary per Association (i.e., the TCB)
//Peer : Tag value to be sent in every packet and is received
//Verification: in the INIT or INIT ACK chunk.
//Tag :
//
//My : Tag expected in every inbound packet and sent in the
//Verification: INIT or INIT ACK chunk.
//
//Tag :
//State : A state variable indicating what state the association
// : is in, i.e., COOKIE-WAIT, COOKIE-ECHOED, ESTABLISHED,
// : SHUTDOWN-PENDING, SHUTDOWN-SENT, SHUTDOWN-RECEIVED,
// : SHUTDOWN-ACK-SENT.
//
// No Closed state is illustrated since if a
// association is Closed its TCB SHOULD be removed.
#[derive(Debug)]
pub struct Association {
    side: Side,
    state: AssociationState,
    handshake_completed: bool,
    max_send_message_size: u32,
    max_receive_message_size: u32,
    inflight_queue_length: usize,
    will_send_shutdown: bool,
    next_stream_generation: u64,
    bytes_received: usize,
    bytes_sent: usize,

    peer_verification_tag: u32,
    my_verification_tag: u32,
    my_next_tsn: u32,
    peer_last_tsn: u32,
    // for RTT measurement
    min_tsn2measure_rtt: u32,
    will_send_forward_tsn: bool,
    will_retransmit_fast: bool,
    will_retransmit_reconfig: bool,

    will_send_shutdown_ack: bool,
    will_send_shutdown_complete: bool,

    // Reconfig
    my_next_rsn: u32,
    reconfigs: FxHashMap<u32, ChunkReconfig>,
    /// Stream ids semantically covered by an outgoing reset. Reset-all stays
    /// compact on the wire, so its completion ids cannot be recovered from the
    /// serialized parameter itself.
    reconfig_reset_streams: FxHashMap<u32, Vec<StreamId>>,
    /// The one request whose Re-configuration Timer is running.
    active_reconfig: Option<u32>,
    /// Application-initiated resets waiting for the active request to finish.
    pending_reset_streams: VecDeque<StreamId>,
    reconfig_requests: FxHashMap<u32, ParamOutgoingResetRequest>,
    /// DATA received above an InProgress reset boundary. A copy is retained
    /// after cumulative TSN processing removes it from `payload_queue`, so it
    /// can remain withheld until the old stream generation is drained.
    deferred_reset_data: FxHashMap<u32, ChunkPayloadData>,
    max_completed_reconfig_rsn: Option<u32>,
    /// Re-configuration Request Sequence Number most recently accepted from
    /// the peer, initialized to the peer's initial TSN minus one.
    peer_last_reconfig_rsn: u32,
    /// Whether `peer_last_reconfig_rsn` has been initialized from an INIT or a
    /// first request in test/manual construction.
    peer_reconfig_rsn_initialized: bool,
    /// Stream ids for which `StreamEvent::Finished` is queued but whose terminal
    /// reset event is still awaited.
    pending_reset_completions: PendingResetCompletions,
    /// Stream ids whose latest completed reset was unsuccessful.
    failed_reset_streams: FxHashSet<StreamId>,
    /// Reset boundaries for generations that cannot finish until an older
    /// application-readable generation with the same stream id is drained.
    retiring_streams: FxHashMap<StreamId, VecDeque<u32>>,
    /// Number of queued Finished events whose generation has actually drained.
    ready_stream_finishes: PendingResetCompletions,
    /// Finished events already delivered to the application and therefore
    /// eligible to be followed by one terminal reset event.
    delivered_stream_finishes: PendingResetCompletions,
    /// Forward-TSN updates that belong to a successor hidden behind a
    /// still-readable stream generation.
    deferred_forward_tsns: FxHashMap<StreamId, VecDeque<DeferredForwardTsn>>,

    // Non-RFC internal data
    remote_addr: SocketAddr,
    local_ip: Option<IpAddr>,
    source_port: u16,
    destination_port: u16,
    my_max_num_inbound_streams: u16,
    my_max_num_outbound_streams: u16,
    my_cookie: Option<ParamStateCookie>,

    payload_queue: PayloadQueue,
    outbound_queue: OutboundQueue,
    control_queue: VecDeque<Packet>,
    stream_queue: VecDeque<u16>,

    pub(crate) mtu: u32,
    // max DATA chunk payload size
    max_payload_size: u32,
    cumulative_tsn_ack_point: u32,
    advanced_peer_tsn_ack_point: u32,
    use_forward_tsn: bool,

    pub(crate) rto_mgr: RtoManager,
    timers: TimerTable,

    // Congestion control parameters
    max_receive_buffer_size: u32,
    // my congestion window size
    pub(crate) cwnd: u32,
    // calculated peer's receiver windows size
    rwnd: u32,
    // slow start threshold
    pub(crate) ssthresh: u32,
    partial_bytes_acked: u32,
    pub(crate) in_fast_recovery: bool,
    fast_recover_exit_point: u32,

    // Chunks stored for retransmission
    stored_init: Option<ChunkInit>,
    stored_cookie_echo: Option<ChunkCookieEcho>,
    pub(crate) streams: FxHashMap<StreamId, StreamState>,

    events: VecDeque<Event>,
    endpoint_events: VecDeque<EndpointEventInner>,
    error: Option<AssociationError>,

    // per inbound packet context
    delayed_ack_triggered: bool,
    immediate_ack_triggered: bool,

    pub(crate) stats: AssociationStats,
    ack_state: AckState,

    // for testing
    pub(crate) ack_mode: AckMode,
}

impl Default for Association {
    fn default() -> Self {
        Association {
            side: Side::default(),
            state: AssociationState::default(),
            handshake_completed: false,
            max_send_message_size: 0,
            max_receive_message_size: 0,
            inflight_queue_length: 0,
            will_send_shutdown: false,
            next_stream_generation: 0,
            bytes_received: 0,
            bytes_sent: 0,

            peer_verification_tag: 0,
            my_verification_tag: 0,
            my_next_tsn: 0,
            peer_last_tsn: 0,
            // for RTT measurement
            min_tsn2measure_rtt: 0,
            will_send_forward_tsn: false,
            will_retransmit_fast: false,
            will_retransmit_reconfig: false,

            will_send_shutdown_ack: false,
            will_send_shutdown_complete: false,

            // Reconfig
            my_next_rsn: 0,
            reconfigs: FxHashMap::default(),
            reconfig_reset_streams: FxHashMap::default(),
            active_reconfig: None,
            pending_reset_streams: VecDeque::default(),
            reconfig_requests: FxHashMap::default(),
            deferred_reset_data: FxHashMap::default(),
            max_completed_reconfig_rsn: None,
            peer_last_reconfig_rsn: 0,
            peer_reconfig_rsn_initialized: false,
            pending_reset_completions: PendingResetCompletions::default(),
            failed_reset_streams: FxHashSet::default(),
            retiring_streams: FxHashMap::default(),
            ready_stream_finishes: PendingResetCompletions::default(),
            delivered_stream_finishes: PendingResetCompletions::default(),
            deferred_forward_tsns: FxHashMap::default(),

            // Non-RFC internal data
            remote_addr: SocketAddr::from_str("0.0.0.0:0").unwrap(),
            local_ip: None,
            source_port: 0,
            destination_port: 0,
            my_max_num_inbound_streams: 0,
            my_max_num_outbound_streams: 0,
            my_cookie: None,

            payload_queue: PayloadQueue::default(),
            outbound_queue: OutboundQueue::default(),
            control_queue: VecDeque::default(),
            stream_queue: VecDeque::default(),

            mtu: 0,
            // max DATA chunk payload size
            max_payload_size: 0,
            cumulative_tsn_ack_point: 0,
            advanced_peer_tsn_ack_point: 0,
            use_forward_tsn: false,

            rto_mgr: RtoManager::default(),
            timers: TimerTable::default(),

            // Congestion control parameters
            max_receive_buffer_size: 0,
            // my congestion window size
            cwnd: 0,
            // calculated peer's receiver windows size
            rwnd: 0,
            // slow start threshold
            ssthresh: 0,
            partial_bytes_acked: 0,
            in_fast_recovery: false,
            fast_recover_exit_point: 0,

            // Chunks stored for retransmission
            stored_init: None,
            stored_cookie_echo: None,
            streams: FxHashMap::default(),

            events: VecDeque::default(),
            endpoint_events: VecDeque::default(),
            error: None,

            // per inbound packet context
            delayed_ack_triggered: false,
            immediate_ack_triggered: false,

            stats: AssociationStats::default(),
            ack_state: AckState::default(),

            // for testing
            ack_mode: AckMode::default(),
        }
    }
}

impl Association {
    fn new_common(
        config: Arc<TransportConfig>,
        max_payload_size: u32,
        remote_addr: SocketAddr,
        local_ip: Option<IpAddr>,
        side: Side,
        verification_tag: u32,
        initial_tsn: u32,
    ) -> Self {
        // It's a bit strange, but we're going backwards from the calculation in
        // config.rs to get max_payload_size from INITIAL_MTU.
        let mtu = max_payload_size + COMMON_HEADER_SIZE + DATA_CHUNK_HEADER_SIZE;

        // RFC 4960 Sec 7.2.1
        // The initial cwnd before DATA transmission or after a sufficiently
        // long idle period MUST be set to min(4*MTU, max (2*MTU, 4380bytes)).
        let cwnd = (4 * mtu).min((2 * mtu).max(4380));

        Association {
            side,
            handshake_completed: false,
            max_receive_buffer_size: config.max_receive_buffer_size(),
            max_send_message_size: config.max_send_message_size(),
            max_receive_message_size: config.max_receive_message_size(),
            my_max_num_outbound_streams: config.max_num_outbound_streams(),
            my_max_num_inbound_streams: config.max_num_inbound_streams(),
            max_payload_size,

            rto_mgr: RtoManager::new(
                config.rto_initial_ms(),
                config.rto_min_ms(),
                config.rto_max_ms(),
            ),
            timers: TimerTable::new(
                config.max_init_retransmits(),
                config.max_data_retransmits(),
                config.rto_max_ms(),
            ),

            mtu,
            cwnd,
            remote_addr,
            local_ip,

            source_port: config.local_port(),
            destination_port: config.remote_port(),

            my_verification_tag: verification_tag,
            my_next_tsn: initial_tsn,
            my_next_rsn: initial_tsn,
            min_tsn2measure_rtt: initial_tsn,
            cumulative_tsn_ack_point: initial_tsn.wrapping_sub(1),
            advanced_peer_tsn_ack_point: initial_tsn.wrapping_sub(1),
            error: None,

            ..Default::default()
        }
    }

    pub(crate) fn new(
        server_config: Option<Arc<ServerConfig>>,
        config: Arc<TransportConfig>,
        max_payload_size: u32,
        local_aid: AssociationId,
        remote_addr: SocketAddr,
        local_ip: Option<IpAddr>,
        now: Instant,
    ) -> Self {
        let side = if server_config.is_some() {
            Side::Server
        } else {
            Side::Client
        };

        let tsn = random::<NonZeroU32>().get();

        let mut this = Self::new_common(
            config,
            max_payload_size,
            remote_addr,
            local_ip,
            side,
            local_aid,
            tsn,
        );

        if side.is_client() {
            let mut init = ChunkInit {
                initial_tsn: this.my_next_tsn,
                num_outbound_streams: this.my_max_num_outbound_streams,
                num_inbound_streams: this.my_max_num_inbound_streams,
                initiate_tag: this.my_verification_tag,
                advertised_receiver_window_credit: this.max_receive_buffer_size,
                ..Default::default()
            };
            init.set_supported_extensions();

            this.set_state(AssociationState::CookieWait);
            this.stored_init = Some(init);
            let _ = this.send_init();
            this.timers
                .start(Timer::T1Init, now, this.rto_mgr.get_rto());
        }

        this
    }

    /// Creates a new association using out-of-band exchanged SNAP tokens (INIT chunks).
    ///
    /// This allows skipping the SCTP 4-way handshake (RFC 4960 Section 5.1)
    /// by exchanging tokens out-of-band (e.g., via a signaling channel
    /// using SDP `a=sctp-init`). The association immediately transitions to
    /// the ESTABLISHED state.
    ///
    /// **Note:** When using SNAP, **both** peers must call
    /// [`Endpoint::connect`](crate::Endpoint::connect). There is no
    /// server-side SNAP via [`Endpoint::handle`](crate::Endpoint::handle).
    ///
    /// See [draft-hancke-tsvwg-snap-01](https://datatracker.ietf.org/doc/draft-hancke-tsvwg-snap/).
    ///
    /// # Arguments
    /// * `config` - Transport configuration.
    /// * `max_payload_size` - Maximum payload size.
    /// * `remote_addr` - Remote socket address.
    /// * `local_ip` - Optional local IP address.
    /// * `local_init` - Parsed local token (INIT chunk).
    /// * `remote_init` - Parsed remote token (INIT chunk).
    ///
    /// # Returns
    /// A new association in the ESTABLISHED state, or an error if the
    /// tokens are invalid.
    pub(crate) fn new_with_out_of_band_init(
        config: Arc<TransportConfig>,
        max_payload_size: u32,
        remote_addr: SocketAddr,
        local_ip: Option<IpAddr>,
        local_init: ChunkInit,
        remote_init: ChunkInit,
    ) -> Result<Self> {
        // Derive side deterministically: the peer with the lower initiate_tag
        // acts as server so that log lines are distinguishable. This is purely
        // cosmetic — equal tags are impossible because the caller in
        // `connect_with_snap` rejects that case with `AidCollision`.
        let side = if local_init.initiate_tag <= remote_init.initiate_tag {
            Side::Server
        } else {
            Side::Client
        };

        // Use the TSN from our local INIT chunk
        let tsn = local_init.initial_tsn;

        let mut this = Self::new_common(
            config.clone(),
            max_payload_size,
            remote_addr,
            local_ip,
            side,
            local_init.initiate_tag,
            tsn,
        );

        // Negotiate stream counts: use the smaller of our config limit and
        // what the remote offers (RFC 4960 §5.1.1 cross-negotiation).
        this.my_max_num_inbound_streams = core::cmp::min(
            config.max_num_inbound_streams(),
            remote_init.num_outbound_streams,
        );
        this.my_max_num_outbound_streams = core::cmp::min(
            config.max_num_outbound_streams(),
            remote_init.num_inbound_streams,
        );

        this.peer_verification_tag = remote_init.initiate_tag;

        this.handshake_completed = true;

        this.apply_remote_init_params(
            remote_init.initial_tsn,
            remote_init.advertised_receiver_window_credit,
            &remote_init.params,
            "out-of-band init",
        );

        // Set state to ESTABLISHED - out-of-band init skips the handshake
        this.set_state(AssociationState::Established);
        this.events.push_back(Event::Connected);

        debug!(
            "[{}] out-of-band init association established: my_tag={:#x} peer_tag={:#x} tsn={}",
            this.side, this.my_verification_tag, this.peer_verification_tag, this.my_next_tsn
        );

        Ok(this)
    }

    /// Returns application-facing event
    ///
    /// Associations should be polled for events after:
    /// - a call was made to `handle_event`
    /// - a call was made to `handle_timeout`
    #[must_use]
    pub fn poll(&mut self) -> Option<Event> {
        // A reset boundary can become cumulative after its DATA was made
        // readable. Keep events for that stream generation behind Finished,
        // while allowing unrelated association and stream events to progress.
        let mut blocked_streams = FxHashSet::default();
        let mut selected = None;
        for (index, event) in self.events.iter().enumerate() {
            if let Event::Stream(StreamEvent::Finished { id }) = event {
                if self.state != AssociationState::Closed
                    && !self.ready_stream_finishes.contains(id)
                {
                    blocked_streams.insert(*id);
                    continue;
                }
            }

            if let Event::Stream(stream_event) = event {
                let stream_id = match stream_event {
                    StreamEvent::Opened { id }
                    | StreamEvent::Readable { id }
                    | StreamEvent::Writable { id }
                    | StreamEvent::Finished { id }
                    | StreamEvent::ResetComplete { id }
                    | StreamEvent::ResetFailed { id, .. }
                    | StreamEvent::Stopped { id, .. }
                    | StreamEvent::BufferedAmountLow { id }
                    | StreamEvent::BufferedAmountHigh { id } => Some(*id),
                    StreamEvent::Available => None,
                };
                if let Some(stream_id) = stream_id {
                    if blocked_streams.contains(&stream_id) {
                        let terminal_for_delivered_generation =
                            matches!(
                                stream_event,
                                StreamEvent::ResetComplete { .. } | StreamEvent::ResetFailed { .. }
                            ) && self.delivered_stream_finishes.contains(&stream_id);
                        let readable_current_generation = matches!(
                            stream_event,
                            StreamEvent::Opened { .. } | StreamEvent::Readable { .. }
                        );
                        if !terminal_for_delivered_generation && !readable_current_generation {
                            continue;
                        }
                    }
                }
            }

            selected = Some(index);
            break;
        }

        if let Some(index) = selected {
            let x = self
                .events
                .remove(index)
                .expect("selected event must exist");
            match &x {
                Event::Stream(StreamEvent::Finished { id }) => {
                    self.ready_stream_finishes.take_one(*id);
                    if self.state != AssociationState::Closed {
                        self.delivered_stream_finishes.insert(*id);
                    }
                }
                Event::Stream(
                    StreamEvent::ResetComplete { id } | StreamEvent::ResetFailed { id, .. },
                ) => {
                    self.delivered_stream_finishes.take_one(*id);
                }
                _ => {}
            }
            return Some(x);
        }

        /*TODO: if let Some(event) = self.streams.poll() {
            return Some(Event::Stream(event));
        }*/

        if let Some(err) = self.error.take() {
            return Some(Event::HandshakeFailed { reason: err });
        }

        None
    }

    /// Return endpoint-facing event
    #[must_use]
    pub fn poll_endpoint_event(&mut self) -> Option<EndpointEvent> {
        self.endpoint_events.pop_front().map(EndpointEvent)
    }

    /// Returns the next time at which `handle_timeout` should be called
    ///
    /// The value returned may change after:
    /// - the application performed some I/O on the association
    /// - a call was made to `handle_transmit`
    /// - a call to `poll_transmit` returned `Some`
    /// - a call was made to `handle_timeout`
    #[must_use]
    pub fn poll_timeout(&self) -> Option<Instant> {
        self.timers.next_timeout()
    }

    /// Returns packets to transmit
    ///
    /// Associations should be polled for transmit after:
    /// - the application performed some I/O on the Association
    /// - a call was made to `handle_event`
    /// - a call was made to `handle_timeout`
    #[must_use]
    pub fn poll_transmit(&mut self, now: Instant) -> Option<Transmit> {
        let (contents, _) = self.gather_outbound(now);
        if contents.is_empty() {
            None
        } else {
            trace!(
                "[{}] sending {} bytes (total {} datagrams)",
                self.side,
                contents.iter().fold(0, |l, c| l + c.len()),
                contents.len()
            );
            Some(Transmit {
                now,
                remote: self.remote_addr,
                payload: Payload::RawEncode(contents),
                ecn: None,
                local_ip: self.local_ip,
            })
        }
    }

    /// Process timer expirations
    ///
    /// Executes protocol logic, potentially preparing signals (including application `Event`s,
    /// `EndpointEvent`s and outgoing datagrams) that should be extracted through the relevant
    /// methods.
    ///
    /// It is most efficient to call this immediately after the system clock reaches the latest
    /// `Instant` that was output by `poll_timeout`; however spurious extra calls will simply
    /// no-op and therefore are safe.
    pub fn handle_timeout(&mut self, now: Instant) {
        for &timer in &Timer::VALUES {
            let (expired, failure, n_rtos) = self.timers.is_expired(timer, now);
            if !expired {
                continue;
            }
            self.timers.set(timer, None);

            if timer == Timer::Ack {
                self.on_ack_timeout();
            } else if failure {
                self.on_retransmission_failure(timer);
            } else {
                self.on_retransmission_timeout(timer, n_rtos);
                self.timers.start(timer, now, self.rto_mgr.get_rto());
            }
        }
    }

    /// Process `AssociationEvent`s generated by the associated `Endpoint`
    ///
    /// Will execute protocol logic upon receipt of an association event, in turn preparing signals
    /// (including application `Event`s, `EndpointEvent`s and outgoing datagrams) that should be
    /// extracted through the relevant methods.
    pub fn handle_event(&mut self, event: AssociationEvent) {
        match event.0 {
            AssociationEventInner::Datagram(transmit) => {
                // If this packet could initiate a migration and we're a client or a server that
                // forbids migration, drop the datagram. This could be relaxed to heuristically
                // permit NAT-rebinding-like migration.
                /*TODO:if remote != self.remote && self.server_config.as_ref().map_or(true, |x| !x.migration)
                {
                    trace!("discarding packet from unrecognized peer {}", remote);
                    return;
                }*/

                if let Payload::PartialDecode(partial_decode) = transmit.payload {
                    trace!(
                        "[{}] receiving {} bytes",
                        self.side,
                        COMMON_HEADER_SIZE as usize + partial_decode.remaining.len()
                    );

                    let pkt = match partial_decode.finish() {
                        Ok(p) => p,
                        Err(err) => {
                            warn!("[{}] unable to parse SCTP packet {}", self.side, err);
                            return;
                        }
                    };

                    if let Err(err) = self.handle_inbound(pkt, transmit.now) {
                        error!("handle_inbound got err: {}", err);
                        if err == Error::ErrReceiveBufferExhausted {
                            self.abort_receive_capacity();
                        } else {
                            let _ = self.close();
                        }
                    }
                } else {
                    trace!("discarding invalid partial_decode");
                }
            } //TODO:
        }
    }

    fn receive_capacity_exhausted(&self) -> bool {
        self.get_my_receiver_window_credit() == 0
            && (!self.deferred_reset_data.is_empty()
                || self
                    .streams
                    .values()
                    .any(|s| s.get_num_bytes_in_reassembly_queue() > 0))
            && self.payload_queue.is_empty()
            && !self.streams.values().any(|s| {
                matches!(
                    s.state,
                    RecvSendState::Readable | RecvSendState::ReadWritable
                ) && s.reassembly_queue.is_readable()
            })
    }

    fn abort_receive_capacity(&mut self) {
        let _ = self.close_with_reason(AssociationError::ReceiveBufferExhausted);
        let abort = ChunkAbort {
            error_causes: vec![crate::chunk::ErrorCause {
                code: crate::chunk::OUT_OF_RESOURCE,
                raw: Bytes::new(),
            }],
        };
        self.control_queue
            .push_back(self.create_packet(vec![Box::new(abort)]));
    }

    /// Returns Association statistics
    pub fn stats(&self) -> AssociationStats {
        self.stats
    }

    /// Whether the Association is in the process of being established
    ///
    /// If this returns `false`, the Association may be either established or closed, signaled by the
    /// emission of a `Connected` or `AssociationLost` message respectively.
    pub fn is_handshaking(&self) -> bool {
        !self.handshake_completed
    }

    /// Whether the Association is closed
    ///
    /// Closed Associations cannot transport any further data. An association becomes closed when
    /// either peer application intentionally closes it, or when either transport layer detects an
    /// error such as a time-out or certificate validation failure.
    ///
    /// A `AssociationLost` event is emitted with details when the association becomes closed.
    pub fn is_closed(&self) -> bool {
        self.state == AssociationState::Closed
    }

    /// Whether the Association has started SCTP shutdown, but is not closed yet
    ///
    /// Closing Associations may still need polling, timer-driven retransmission, and packet output
    /// before they become fully closed.
    pub fn is_closing(&self) -> bool {
        self.state.is_closing()
    }

    /// Whether there is no longer any need to keep the association around
    ///
    /// Closed associations become drained after a brief timeout to absorb any remaining in-flight
    /// packets from the peer. All drained associations have been closed.
    pub fn is_drained(&self) -> bool {
        self.state.is_drained()
    }

    /// Look up whether we're the client or server of this Association
    pub fn side(&self) -> Side {
        self.side
    }

    /// The latest socket address for this Association's peer
    pub fn remote_addr(&self) -> SocketAddr {
        self.remote_addr
    }

    /// Current best estimate of this Association's latency (round-trip-time)
    pub fn rtt(&self) -> Duration {
        Duration::from_millis(self.rto_mgr.get_rto())
    }

    /// The local IP address which was used when the peer established
    /// the association
    ///
    /// This can be different from the address the endpoint is bound to, in case
    /// the endpoint is bound to a wildcard address like `0.0.0.0` or `::`.
    ///
    /// This will return `None` for clients.
    ///
    /// Retrieving the local IP address is currently supported on the following
    /// platforms:
    /// - Linux
    ///
    /// On all non-supported platforms the local IP address will not be available,
    /// and the method will return `None`.
    pub fn local_ip(&self) -> Option<IpAddr> {
        self.local_ip
    }

    /// Shutdown initiates the shutdown sequence. The method blocks until the
    /// shutdown sequence is completed and the association is closed, or until the
    /// passed context is done, in which case the context's error is returned.
    pub fn shutdown(&mut self) -> Result<()> {
        debug!("[{}] closing association..", self.side);

        let state = self.state();
        if state != AssociationState::Established {
            return Err(Error::ErrShutdownNonEstablished);
        }

        // Attempt a graceful shutdown.
        self.set_state(AssociationState::ShutdownPending);

        if self.inflight_queue_length == 0 {
            // No more outstanding, send shutdown.
            self.will_send_shutdown = true;
            self.awake_write_loop();
            self.set_state(AssociationState::ShutdownSent);
        }

        self.endpoint_events.push_back(EndpointEventInner::Drained);

        Ok(())
    }

    /// Close ends the SCTP Association and cleans up any state
    pub fn close(&mut self) -> Result<()> {
        self.close_with_reason(AssociationError::AssociationClosed)
    }

    fn close_with_reason(&mut self, reason: AssociationError) -> Result<()> {
        if self.state() != AssociationState::Closed {
            self.set_state(AssociationState::Closed);

            debug!("[{}] closing association..", self.side);

            self.close_all_timers();

            for si in self.streams.keys().cloned().collect::<Vec<u16>>() {
                self.unregister_stream(si, false);
            }

            // AssociationLost stops any pending  ResetComplete.
            self.pending_reset_completions.clear();
            self.failed_reset_streams.clear();
            self.retiring_streams.clear();
            self.ready_stream_finishes.clear();
            self.delivered_stream_finishes.clear();
            self.deferred_forward_tsns.clear();
            self.pending_reset_streams.clear();
            self.reconfigs.clear();
            self.reconfig_reset_streams.clear();
            self.reconfig_requests.clear();
            self.deferred_reset_data.clear();
            self.control_queue.clear();
            self.active_reconfig = None;
            self.will_retransmit_reconfig = false;

            self.events.push_back(Event::AssociationLost { reason });

            debug!("[{}] association closed", self.side);
            debug!(
                "[{}] stats nDATAs (in) : {}",
                self.side,
                self.stats.get_num_datas()
            );
            debug!(
                "[{}] stats nSACKs (in) : {}",
                self.side,
                self.stats.get_num_sacks()
            );
            debug!(
                "[{}] stats nT3Timeouts : {}",
                self.side,
                self.stats.get_num_t3timeouts()
            );
            debug!(
                "[{}] stats nAckTimeouts: {}",
                self.side,
                self.stats.get_num_ack_timeouts()
            );
            debug!(
                "[{}] stats nFastRetrans: {}",
                self.side,
                self.stats.get_num_fast_retrans()
            );
        }

        Ok(())
    }

    /// open_stream opens a stream
    pub fn open_stream(
        &mut self,
        stream_identifier: StreamId,
        default_payload_type: PayloadProtocolIdentifier,
    ) -> Result<Stream<'_>> {
        if self.streams.contains_key(&stream_identifier) {
            return Err(Error::ErrStreamAlreadyExist);
        }

        if self.stream_reset_blocked(stream_identifier) {
            return Err(Error::ErrStreamResetPending);
        }

        if let Some(s) = self.create_stream(stream_identifier, false, default_payload_type) {
            Ok(s)
        } else {
            Err(Error::ErrStreamCreateFailed)
        }
    }

    /// accept_stream accepts a stream
    pub fn accept_stream(&mut self) -> Option<Stream<'_>> {
        self.stream_queue
            .pop_front()
            .map(move |stream_identifier| Stream {
                stream_identifier,
                association: self,
            })
    }

    /// stream returns a stream
    pub fn stream(&mut self, stream_identifier: StreamId) -> Result<Stream<'_>> {
        if !self.streams.contains_key(&stream_identifier) {
            Err(Error::ErrStreamNotExisted)
        } else {
            Ok(Stream {
                stream_identifier,
                association: self,
            })
        }
    }

    /// stream_ids returns a list of all active stream identifiers
    pub fn stream_ids(&self) -> Vec<StreamId> {
        self.streams.keys().cloned().collect()
    }

    /// bytes_sent returns the number of bytes sent
    pub(crate) fn bytes_sent(&self) -> usize {
        self.bytes_sent
    }

    /// bytes_received returns the number of bytes received
    pub(crate) fn bytes_received(&self) -> usize {
        self.bytes_received
    }

    /// max_send_message_size returns the maximum message size you can send.
    pub(crate) fn max_send_message_size(&self) -> u32 {
        self.max_send_message_size
    }

    /// set_max_send_message_size sets the maximum message size you can send.
    pub fn set_max_send_message_size(&mut self, value: u32) {
        self.max_send_message_size = value;
    }

    /// max_receive_message_size returns the maximum message size accepted.
    pub(crate) fn max_receive_message_size(&self) -> u32 {
        self.max_receive_message_size
    }

    /// set_max_receive_message_size sets the maximum message size accepted.
    pub(crate) fn set_max_receive_message_size(&mut self, value: u32) {
        self.max_receive_message_size = value;
    }

    /// max_message_size returns the maximum message size you can send.
    #[deprecated(note = "Use max_send_message_size instead")]
    pub(crate) fn max_message_size(&self) -> u32 {
        self.max_send_message_size()
    }

    /// set_max_message_size sets the maximum message size you can send.
    #[deprecated(note = "Use set_max_send_message_size instead")]
    pub(crate) fn set_max_message_size(&mut self, value: u32) {
        self.set_max_send_message_size(value)
    }

    /// Push one [`StreamEvent::ResetComplete`] for each candidate id whose reset
    /// handshake has completed and `Finished` has already been fired for it.
    fn emit_reset_complete(&mut self, ids: impl IntoIterator<Item = StreamId>) {
        for id in ids {
            let notify = self.pending_reset_completions.take_one(id);
            // Requests are serialized, so a success supersedes any older failure
            // for the same stream id. A newer pending generation still blocks reuse.
            self.failed_reset_streams.remove(&id);
            let reset_blocked = self.stream_reset_blocked(id);
            if !reset_blocked {
                let application_closed = notify
                    && self.streams.get(&id).is_some_and(|stream| {
                        matches!(
                            stream.state,
                            RecvSendState::Closed | RecvSendState::Writable
                        )
                    });
                if application_closed {
                    self.unregister_stream(id, false);
                } else if let Some(stream) = self.streams.get_mut(&id) {
                    // RFC 6525 section 5.2.7 H4 resets the outgoing SSN. Do not
                    // alter `state`: a protocol reset must never undo an
                    // application-level `Stream::finish()`.
                    stream.sequence_number = 0;
                }
            }
            if notify {
                self.events
                    .push_back(Event::Stream(StreamEvent::ResetComplete { id }));
            }
        }
    }

    /// Report a terminal reset failure and keep the stream id quarantined.
    fn emit_reset_failed(
        &mut self,
        ids: impl IntoIterator<Item = StreamId>,
        reason: StreamResetError,
    ) {
        for id in ids {
            self.pending_reset_completions.take_one(id);
            self.failed_reset_streams.insert(id);
            self.events
                .push_back(Event::Stream(StreamEvent::ResetFailed { id, reason }));
        }
    }

    /// Returns true if the given stream ID appears in any pending outgoing
    /// RE-CONFIG that has not yet been acknowledged by the remote peer.
    fn has_pending_reset_for_stream(&self, stream_id: StreamId) -> bool {
        self.reconfigs.values().any(|c| {
            c.param_a
                .iter()
                .chain(c.param_b.iter())
                .find_map(|p| p.as_any().downcast_ref::<ParamOutgoingResetRequest>())
                .is_some_and(|p| Self::reset_request_affects_stream(p, stream_id))
        })
    }

    fn reset_request_affects_stream(
        request: &ParamOutgoingResetRequest,
        stream_id: StreamId,
    ) -> bool {
        request.stream_identifiers.is_empty() || request.stream_identifiers.contains(&stream_id)
    }

    /// Whether the current generation of a stream id is unsafe to send or reuse.
    fn stream_reset_blocked(&self, stream_id: StreamId) -> bool {
        self.pending_reset_completions.contains(&stream_id)
            || self.failed_reset_streams.contains(&stream_id)
            || self.retiring_streams.contains_key(&stream_id)
            || self.stream_reset_in_progress(stream_id)
    }

    /// Whether a request currently prevents assigning new SSNs for this stream.
    fn stream_reset_in_progress(&self, stream_id: StreamId) -> bool {
        self.pending_reset_streams.contains(&stream_id)
            || self.has_pending_reset_for_stream(stream_id)
    }

    fn reconfig_stream_ids(c: &ChunkReconfig) -> Vec<StreamId> {
        c.param_a
            .iter()
            .chain(c.param_b.iter())
            .find_map(|p| p.as_any().downcast_ref::<ParamOutgoingResetRequest>())
            .map(|p| p.stream_identifiers.clone())
            .unwrap_or_default()
    }

    fn packet_reconfig_request_rsn(packet: &Packet) -> Option<u32> {
        packet.chunks.iter().find_map(|chunk| {
            let reconfig = chunk.as_any().downcast_ref::<ChunkReconfig>()?;
            reconfig
                .param_a
                .iter()
                .chain(reconfig.param_b.iter())
                .find_map(|param| {
                    param
                        .as_any()
                        .downcast_ref::<ParamOutgoingResetRequest>()
                        .map(|request| request.reconfig_request_sequence_number)
                })
        })
    }

    fn reconfig_with_sender_last_tsn(c: &ChunkReconfig, sender_last_tsn: u32) -> ChunkReconfig {
        fn update(
            param: &Option<Box<dyn Param + Send + Sync>>,
            sender_last_tsn: u32,
        ) -> Option<Box<dyn Param + Send + Sync>> {
            param.as_ref().map(|param| {
                if let Some(request) = param.as_any().downcast_ref::<ParamOutgoingResetRequest>() {
                    let mut request = request.clone();
                    request.sender_last_tsn = sender_last_tsn;
                    Box::new(request) as Box<dyn Param + Send + Sync>
                } else {
                    param.clone()
                }
            })
        }

        ChunkReconfig {
            param_a: update(&c.param_a, sender_last_tsn),
            param_b: update(&c.param_b, sender_last_tsn),
        }
    }

    fn reconfig_has_pending_data(&self, rsn: u32) -> bool {
        self.reconfigs.get(&rsn).is_some_and(|c| {
            let stream_ids = Self::reconfig_stream_ids(c);
            if stream_ids.is_empty() {
                self.outbound_queue.pending_len() != 0
            } else {
                stream_ids
                    .iter()
                    .any(|id| self.outbound_queue.pending_contains_stream(*id))
            }
        })
    }

    fn refresh_unsent_reconfig(&mut self, rsn: u32) -> Option<Packet> {
        let sender_last_tsn = self.my_next_tsn.wrapping_sub(1);
        let reconfig = self
            .reconfigs
            .get(&rsn)
            .map(|c| Self::reconfig_with_sender_last_tsn(c, sender_last_tsn))?;
        self.reconfigs.insert(rsn, reconfig.clone());
        Some(self.create_packet(vec![Box::new(reconfig)]))
    }

    /// Finish the one request associated with the running Re-configuration Timer.
    fn finish_reconfig(
        &mut self,
        rsn: u32,
        outcome: core::result::Result<(), StreamResetError>,
    ) -> bool {
        if self.active_reconfig != Some(rsn) {
            return false;
        }

        let fallback_ids = self
            .reconfigs
            .remove(&rsn)
            .map(|c| Self::reconfig_stream_ids(&c))
            .unwrap_or_default();
        let ids = self
            .reconfig_reset_streams
            .remove(&rsn)
            .unwrap_or(fallback_ids);
        self.active_reconfig = None;
        self.will_retransmit_reconfig = false;
        self.timers.stop(Timer::Reconfig);

        match outcome {
            Ok(()) => self.emit_reset_complete(ids),
            Err(reason) => self.emit_reset_failed(ids, reason),
        }

        // A buffered request or locally queued reset can now become active.
        self.awake_write_loop();
        true
    }

    /// unregister_stream un-registers a stream from the association
    /// The caller should hold the association write lock.
    fn unregister_stream(&mut self, stream_identifier: StreamId, emit_stream_finished: bool) {
        if let Some(mut s) = self.streams.remove(&stream_identifier) {
            debug!("[{}] unregister_stream {}", self.side, stream_identifier);
            self.events.push_back(Event::DataChannelStatsFinal(s.stats));
            s.state = RecvSendState::Closed;
            if emit_stream_finished {
                self.queue_stream_finished(stream_identifier, true);
            }
        }
    }

    fn queue_stream_finished(&mut self, stream_identifier: StreamId, ready: bool) {
        self.events.push_back(Event::Stream(StreamEvent::Finished {
            id: stream_identifier,
        }));
        // Every Finished owes a terminal reset event, even if the peer
        // re-creates the stream id before the handshake completes.
        self.pending_reset_completions.insert(stream_identifier);
        if ready {
            self.ready_stream_finishes.insert(stream_identifier);
        }
    }

    /// Queue the end of an incoming stream generation while preserving any
    /// already-acknowledged DATA until the application drains it.
    fn retire_stream(&mut self, stream_identifier: StreamId, sender_last_tsn: u32) {
        if let Some(boundaries) = self.retiring_streams.get_mut(&stream_identifier) {
            boundaries.push_back(sender_last_tsn);
            if let Some(updates) = self.deferred_forward_tsns.get_mut(&stream_identifier) {
                for update in updates {
                    if update.generation_boundary.is_none() {
                        update.generation_boundary = Some(sender_last_tsn);
                    }
                }
            }
            self.queue_stream_finished(stream_identifier, false);
            return;
        }

        let has_readable_data = self.streams.get(&stream_identifier).is_some_and(|stream| {
            matches!(
                stream.state,
                RecvSendState::Readable | RecvSendState::ReadWritable
            ) && stream.get_num_bytes_in_reassembly_queue() != 0
        });

        if !has_readable_data {
            self.deferred_forward_tsns.remove(&stream_identifier);
            self.unregister_stream(stream_identifier, true);
            return;
        }

        if let Some(updates) = self.deferred_forward_tsns.get_mut(&stream_identifier) {
            for update in updates {
                if update.generation_boundary.is_none() {
                    update.generation_boundary = Some(sender_last_tsn);
                }
            }
        }
        let mut boundaries = VecDeque::new();
        boundaries.push_back(sender_last_tsn);
        self.retiring_streams.insert(stream_identifier, boundaries);
        self.queue_stream_finished(stream_identifier, false);
    }

    /// Drop a drained old generation and release DATA held for its successor.
    pub(crate) fn finish_retiring_stream(&mut self, stream_identifier: StreamId) -> Result<()> {
        if !self.retiring_streams.contains_key(&stream_identifier) {
            return Ok(());
        }

        let inherited_state = self
            .streams
            .get(&stream_identifier)
            .map(|stream| stream.state)
            .unwrap_or(RecvSendState::ReadWritable);
        if let Some(mut stream) = self.streams.remove(&stream_identifier) {
            self.events
                .push_back(Event::DataChannelStatsFinal(stream.stats));
            stream.state = RecvSendState::Closed;
        }

        loop {
            let (completed_boundary, next_boundary) = {
                let boundaries = self
                    .retiring_streams
                    .get_mut(&stream_identifier)
                    .expect("retiring stream must retain a boundary");
                let completed = boundaries
                    .pop_front()
                    .expect("retiring stream must have a current generation");
                (completed, boundaries.front().copied())
            };
            self.discard_deferred_forward_tsns_through(stream_identifier, completed_boundary);
            self.ready_stream_finishes.insert(stream_identifier);

            let Some(next_boundary) = next_boundary else {
                self.retiring_streams.remove(&stream_identifier);
                let result = self.release_deferred_reset_data();
                self.inherit_stream_state(stream_identifier, inherited_state);
                return result;
            };

            self.release_deferred_generation_data(
                stream_identifier,
                completed_boundary,
                next_boundary,
            )?;
            self.inherit_stream_state(stream_identifier, inherited_state);

            let has_unread_data = self
                .streams
                .get(&stream_identifier)
                .is_some_and(|stream| stream.get_num_bytes_in_reassembly_queue() != 0);
            if has_unread_data {
                return Ok(());
            }

            // This generation contained no deliverable DATA. Its already-queued
            // Finished event can become ready immediately, and any forwarding
            // state scoped to it must not leak into the following generation.
            if let Some(mut stream) = self.streams.remove(&stream_identifier) {
                self.events
                    .push_back(Event::DataChannelStatsFinal(stream.stats));
                stream.state = RecvSendState::Closed;
            }
            self.discard_deferred_forward_tsns_through(stream_identifier, next_boundary);
        }
    }

    fn inherit_stream_state(
        &mut self,
        stream_identifier: StreamId,
        inherited_state: RecvSendState,
    ) {
        if let Some(stream) = self.streams.get_mut(&stream_identifier) {
            stream.state = ((stream.state as u8) & (inherited_state as u8)).into();
        }
    }

    /// Close every retained incoming generation without replaying its DATA.
    /// The current StreamState is kept so an application-owned write half and
    /// its configuration survive a read-side stop.
    pub(crate) fn discard_retiring_streams(&mut self, stream_identifier: StreamId) {
        let Some(boundaries) = self.retiring_streams.remove(&stream_identifier) else {
            return;
        };

        for _ in boundaries {
            self.ready_stream_finishes.insert(stream_identifier);
        }
        let discarded_tsns: Vec<u32> = self
            .deferred_reset_data
            .iter()
            .filter_map(|(tsn, chunk)| {
                (chunk.stream_identifier == stream_identifier).then_some(*tsn)
            })
            .collect();
        for tsn in discarded_tsns {
            self.payload_queue.mark_as_acked(tsn);
        }
        self.deferred_reset_data
            .retain(|_, chunk| chunk.stream_identifier != stream_identifier);
        self.deferred_forward_tsns.remove(&stream_identifier);
        self.events.retain(|event| {
            !matches!(
                event,
                Event::Stream(
                    StreamEvent::Opened { id } | StreamEvent::Readable { id }
                ) if *id == stream_identifier
            )
        });

        if let Some(stream) = self.streams.get_mut(&stream_identifier) {
            stream.reassembly_queue =
                ReassemblyQueue::new(stream_identifier, self.max_receive_message_size);
        }
        if !self.stream_reset_blocked(stream_identifier) {
            self.unregister_stream(stream_identifier, false);
        }
    }

    /// set_state atomically sets the state of the Association.
    fn set_state(&mut self, new_state: AssociationState) {
        if new_state != self.state {
            debug!(
                "[{}] state change: '{}' => '{}'",
                self.side, self.state, new_state,
            );
        }
        self.state = new_state;
    }

    /// state atomically returns the state of the Association.
    pub(crate) fn state(&self) -> AssociationState {
        self.state
    }

    /// Apply common remote-side parameters from an INIT or INIT-ACK chunk.
    ///
    /// Sets `peer_last_tsn`, `rwnd`, `ssthresh`, and `use_forward_tsn` based
    /// on the remote peer's initial TSN, advertised window, and supported
    /// extensions. Used by `handle_init`, `handle_init_ack`, and
    /// `new_with_out_of_band_init`.
    fn apply_remote_init_params(
        &mut self,
        initial_tsn: u32,
        advertised_receiver_window_credit: u32,
        params: &[Box<dyn Param + Send + Sync>],
        context: &str,
    ) {
        // RFC 4960 §13.2: peer_last_tsn is the peer's initial TSN minus one.
        self.peer_last_tsn = initial_tsn.wrapping_sub(1);
        // RFC 6525 §4: the peer's first request sequence number is its initial
        // TSN, so A4's initial response value is one less than that.
        self.peer_last_reconfig_rsn = initial_tsn.wrapping_sub(1);
        self.peer_reconfig_rsn_initialized = true;

        self.rwnd = advertised_receiver_window_credit;
        debug!("[{}] initial rwnd={}", self.side, self.rwnd);

        // RFC 4960 Sec 7.2.1
        //  o  The initial value of ssthresh MAY be arbitrarily high (for
        //     example, implementations MAY use the size of the receiver
        //     advertised window).
        self.ssthresh = self.rwnd;
        trace!(
            "[{}] updated cwnd={} ssthresh={} inflight={} ({})",
            self.side,
            self.cwnd,
            self.ssthresh,
            self.outbound_queue.inflight_bytes(),
            context,
        );

        for param in params {
            if let Some(v) = param.as_any().downcast_ref::<ParamSupportedExtensions>() {
                for t in &v.chunk_types {
                    if *t == CT_FORWARD_TSN {
                        debug!("[{}] use ForwardTSN (on {})", self.side, context);
                        self.use_forward_tsn = true;
                    }
                }
            }
        }
        if !self.use_forward_tsn {
            warn!("[{}] not using ForwardTSN (on {})", self.side, context);
        }
    }

    /// caller must hold self.lock
    fn send_init(&mut self) -> Result<()> {
        if let Some(stored_init) = &self.stored_init {
            debug!("[{}] sending INIT", self.side);

            let outbound = Packet {
                common_header: CommonHeader {
                    source_port: self.source_port,
                    destination_port: self.destination_port,
                    verification_tag: self.peer_verification_tag,
                },
                chunks: vec![Box::new(stored_init.clone())],
            };

            self.control_queue.push_back(outbound);
            self.awake_write_loop();

            Ok(())
        } else {
            Err(Error::ErrInitNotStoredToSend)
        }
    }

    /// caller must hold self.lock
    fn send_cookie_echo(&mut self) -> Result<()> {
        if let Some(stored_cookie_echo) = &self.stored_cookie_echo {
            debug!("[{}] sending COOKIE-ECHO", self.side);

            let outbound = Packet {
                common_header: CommonHeader {
                    source_port: self.source_port,
                    destination_port: self.destination_port,
                    verification_tag: self.peer_verification_tag,
                },
                chunks: vec![Box::new(stored_cookie_echo.clone())],
            };

            self.control_queue.push_back(outbound);
            self.awake_write_loop();

            Ok(())
        } else {
            Err(Error::ErrCookieEchoNotStoredToSend)
        }
    }

    /// handle_inbound parses incoming raw packets
    fn handle_inbound(&mut self, p: Packet, now: Instant) -> Result<()> {
        if let Err(err) = p.check_packet() {
            warn!("[{}] failed validating packet {}", self.side, err);
            return Ok(());
        }

        self.handle_chunk_start();

        for c in &p.chunks {
            self.handle_chunk(&p, c, now)?;
        }

        self.handle_chunk_end(now);

        Ok(())
    }

    fn handle_chunk_start(&mut self) {
        self.delayed_ack_triggered = false;
        self.immediate_ack_triggered = false;
    }

    fn handle_chunk_end(&mut self, now: Instant) {
        if self.immediate_ack_triggered {
            self.ack_state = AckState::Immediate;
            self.timers.stop(Timer::Ack);
            self.awake_write_loop();
        } else if self.delayed_ack_triggered {
            // Will send delayed ack in the next ack timeout
            self.ack_state = AckState::Delay;
            self.timers.start(Timer::Ack, now, ACK_INTERVAL);
        }
    }

    #[allow(clippy::borrowed_box)]
    fn handle_chunk(
        &mut self,
        p: &Packet,
        chunk: &Box<dyn Chunk + Send + Sync>,
        now: Instant,
    ) -> Result<()> {
        chunk.check()?;
        let chunk_any = chunk.as_any();
        let packets = if let Some(c) = chunk_any.downcast_ref::<ChunkInit>() {
            if c.is_ack {
                self.handle_init_ack(p, c, now)?
            } else {
                self.handle_init(p, c)?
            }
        } else if let Some(c) = chunk_any.downcast_ref::<ChunkAbort>() {
            let mut err_str = String::new();
            for e in &c.error_causes {
                if matches!(e.code, USER_INITIATED_ABORT) {
                    debug!("User initiated abort received");
                    let _ = self.close();
                    return Ok(());
                }
                err_str += &format!("({})", e);
            }
            return Err(Error::ErrAbortChunk(err_str));
        } else if let Some(c) = chunk_any.downcast_ref::<ChunkError>() {
            let mut err_str = String::new();
            for e in &c.error_causes {
                err_str += &format!("({})", e);
            }
            return Err(Error::ErrAbortChunk(err_str));
        } else if let Some(c) = chunk_any.downcast_ref::<ChunkHeartbeat>() {
            self.handle_heartbeat(c)?
        } else if let Some(c) = chunk_any.downcast_ref::<ChunkCookieEcho>() {
            self.handle_cookie_echo(c)?
        } else if chunk_any.downcast_ref::<ChunkCookieAck>().is_some() {
            self.handle_cookie_ack()?
        } else if let Some(c) = chunk_any.downcast_ref::<ChunkPayloadData>() {
            self.handle_data(c)?
        } else if let Some(c) = chunk_any.downcast_ref::<ChunkSelectiveAck>() {
            self.handle_sack(c, now)?
        } else if let Some(c) = chunk_any.downcast_ref::<ChunkReconfig>() {
            self.handle_reconfig(c)?
        } else if let Some(c) = chunk_any.downcast_ref::<ChunkForwardTsn>() {
            self.handle_forward_tsn(c)?
        } else if let Some(c) = chunk_any.downcast_ref::<ChunkIForwardTsn>() {
            self.handle_i_forward_tsn(c)?
        } else if let Some(c) = chunk_any.downcast_ref::<ChunkShutdown>() {
            self.handle_shutdown(c)?
        } else if let Some(c) = chunk_any.downcast_ref::<ChunkShutdownAck>() {
            self.handle_shutdown_ack(c)?
        } else if let Some(c) = chunk_any.downcast_ref::<ChunkShutdownComplete>() {
            self.handle_shutdown_complete(c)?
        } else {
            return Err(Error::ErrChunkTypeUnhandled);
        };

        if !packets.is_empty() {
            let mut buf: VecDeque<_> = packets.into_iter().collect();
            self.control_queue.append(&mut buf);
            self.awake_write_loop();
        }

        Ok(())
    }

    fn handle_init(&mut self, p: &Packet, i: &ChunkInit) -> Result<Vec<Packet>> {
        let state = self.state();
        debug!("[{}] chunkInit received in state '{}'", self.side, state);

        // https://tools.ietf.org/html/rfc4960#section-5.2.1
        // Upon receipt of an INIT in the COOKIE-WAIT state, an endpoint MUST
        // respond with an INIT ACK using the same parameters it sent in its
        // original INIT chunk (including its Initiate Tag, unchanged).  When
        // responding, the endpoint MUST send the INIT ACK back to the same
        // address that the original INIT (sent by this endpoint) was sent.

        if state != AssociationState::Closed
            && state != AssociationState::CookieWait
            && state != AssociationState::CookieEchoed
        {
            // 5.2.2.  Unexpected INIT in States Other than CLOSED, COOKIE-ECHOED,
            //        COOKIE-WAIT, and SHUTDOWN-ACK-SENT
            return Err(Error::ErrHandleInitState);
        }

        // Should we be setting any of these permanently until we've ACKed further?
        self.my_max_num_inbound_streams =
            core::cmp::min(i.num_inbound_streams, self.my_max_num_inbound_streams);
        self.my_max_num_outbound_streams =
            core::cmp::min(i.num_outbound_streams, self.my_max_num_outbound_streams);
        self.peer_verification_tag = i.initiate_tag;
        self.source_port = p.common_header.destination_port;
        self.destination_port = p.common_header.source_port;

        self.apply_remote_init_params(
            i.initial_tsn,
            i.advertised_receiver_window_credit,
            &i.params,
            "init",
        );

        let mut outbound = Packet {
            common_header: CommonHeader {
                verification_tag: self.peer_verification_tag,
                source_port: self.source_port,
                destination_port: self.destination_port,
            },
            chunks: vec![],
        };

        let mut init_ack = ChunkInit {
            is_ack: true,
            initial_tsn: self.my_next_tsn,
            num_outbound_streams: self.my_max_num_outbound_streams,
            num_inbound_streams: self.my_max_num_inbound_streams,
            initiate_tag: self.my_verification_tag,
            advertised_receiver_window_credit: self.max_receive_buffer_size,
            ..Default::default()
        };

        if self.my_cookie.is_none() {
            self.my_cookie = Some(ParamStateCookie::new());
        }

        if let Some(my_cookie) = &self.my_cookie {
            init_ack.params = vec![Box::new(my_cookie.clone())];
        }

        init_ack.set_supported_extensions();

        outbound.chunks = vec![Box::new(init_ack)];

        Ok(vec![outbound])
    }

    fn handle_init_ack(
        &mut self,
        p: &Packet,
        i: &ChunkInitAck,
        now: Instant,
    ) -> Result<Vec<Packet>> {
        let state = self.state();
        debug!("[{}] chunkInitAck received in state '{}'", self.side, state);
        if state != AssociationState::CookieWait {
            // RFC 4960
            // 5.2.3.  Unexpected INIT ACK
            //   If an INIT ACK is received by an endpoint in any state other than the
            //   COOKIE-WAIT state, the endpoint should discard the INIT ACK chunk.
            //   An unexpected INIT ACK usually indicates the processing of an old or
            //   duplicated INIT chunk.
            return Ok(vec![]);
        }

        self.my_max_num_inbound_streams =
            core::cmp::min(i.num_inbound_streams, self.my_max_num_inbound_streams);
        self.my_max_num_outbound_streams =
            core::cmp::min(i.num_outbound_streams, self.my_max_num_outbound_streams);
        self.peer_verification_tag = i.initiate_tag;
        if self.source_port != p.common_header.destination_port
            || self.destination_port != p.common_header.source_port
        {
            warn!("[{}] handle_init_ack: port mismatch", self.side);
            return Ok(vec![]);
        }

        self.apply_remote_init_params(
            i.initial_tsn,
            i.advertised_receiver_window_credit,
            &i.params,
            "initAck",
        );

        self.timers.stop(Timer::T1Init);
        self.stored_init = None;

        let cookie_param = i
            .params
            .iter()
            .find_map(|param| param.as_any().downcast_ref::<ParamStateCookie>());

        if let Some(v) = cookie_param {
            self.stored_cookie_echo = Some(ChunkCookieEcho {
                cookie: v.cookie.clone(),
            });

            self.send_cookie_echo()?;

            self.timers
                .start(Timer::T1Cookie, now, self.rto_mgr.get_rto());

            self.set_state(AssociationState::CookieEchoed);

            Ok(vec![])
        } else {
            Err(Error::ErrInitAckNoCookie)
        }
    }

    fn handle_heartbeat(&self, c: &ChunkHeartbeat) -> Result<Vec<Packet>> {
        trace!("[{}] chunkHeartbeat", self.side);
        if let Some(p) = c.params.first() {
            if let Some(hbi) = p.as_any().downcast_ref::<ParamHeartbeatInfo>() {
                return Ok(vec![Packet {
                    common_header: CommonHeader {
                        verification_tag: self.peer_verification_tag,
                        source_port: self.source_port,
                        destination_port: self.destination_port,
                    },
                    chunks: vec![Box::new(ChunkHeartbeatAck {
                        params: vec![Box::new(ParamHeartbeatInfo {
                            heartbeat_information: hbi.heartbeat_information.clone(),
                        })],
                    })],
                }]);
            } else {
                warn!(
                    "[{}] failed to handle Heartbeat, no ParamHeartbeatInfo",
                    self.side,
                );
            }
        }

        Ok(vec![])
    }

    fn handle_cookie_echo(&mut self, c: &ChunkCookieEcho) -> Result<Vec<Packet>> {
        let state = self.state();
        debug!("[{}] COOKIE-ECHO received in state '{}'", self.side, state);

        if let Some(my_cookie) = &self.my_cookie {
            match state {
                AssociationState::Established => {
                    if my_cookie.cookie != c.cookie {
                        return Ok(vec![]);
                    }
                }
                AssociationState::Closed
                | AssociationState::CookieWait
                | AssociationState::CookieEchoed => {
                    if my_cookie.cookie != c.cookie {
                        return Ok(vec![]);
                    }

                    self.timers.stop(Timer::T1Init);
                    self.stored_init = None;

                    self.timers.stop(Timer::T1Cookie);
                    self.stored_cookie_echo = None;

                    self.events.push_back(Event::Connected);
                    self.set_state(AssociationState::Established);
                    self.handshake_completed = true;
                }
                _ => return Ok(vec![]),
            };
        } else {
            debug!("[{}] COOKIE-ECHO received before initialization", self.side);
            return Ok(vec![]);
        }

        Ok(vec![Packet {
            common_header: CommonHeader {
                verification_tag: self.peer_verification_tag,
                source_port: self.source_port,
                destination_port: self.destination_port,
            },
            chunks: vec![Box::new(ChunkCookieAck {})],
        }])
    }

    fn handle_cookie_ack(&mut self) -> Result<Vec<Packet>> {
        let state = self.state();
        debug!("[{}] COOKIE-ACK received in state '{}'", self.side, state);
        if state != AssociationState::CookieEchoed {
            // RFC 4960
            // 5.2.5.  Handle Duplicate COOKIE-ACK.
            //   At any state other than COOKIE-ECHOED, an endpoint should silently
            //   discard a received COOKIE ACK chunk.
            return Ok(vec![]);
        }

        self.timers.stop(Timer::T1Cookie);
        self.stored_cookie_echo = None;

        self.events.push_back(Event::Connected);
        self.set_state(AssociationState::Established);
        self.handshake_completed = true;

        Ok(vec![])
    }

    fn data_is_above_pending_reset(&self, d: &ChunkPayloadData) -> bool {
        self.retiring_streams.contains_key(&d.stream_identifier)
            || self.reconfig_requests.values().any(|request| {
                Self::reset_request_affects_stream(request, d.stream_identifier)
                    && sna32gt(d.tsn, request.sender_last_tsn)
            })
    }

    fn current_reset_boundary(&self, stream_identifier: StreamId) -> Option<u32> {
        self.retiring_streams
            .get(&stream_identifier)
            .and_then(|boundaries| boundaries.front().copied())
    }

    fn pending_reset_boundary(&self, stream_identifier: StreamId) -> Option<u32> {
        self.reconfig_requests
            .values()
            .find(|request| Self::reset_request_affects_stream(request, stream_identifier))
            .map(|request| request.sender_last_tsn)
    }

    fn forward_tsn_applies_to_current_generation(&self, stream_identifier: StreamId) -> bool {
        let current_boundary = self.current_reset_boundary(stream_identifier);
        current_boundary.is_none()
            || current_boundary == self.pending_reset_boundary(stream_identifier)
    }

    fn apply_or_defer_ordered_forward_tsn(
        &mut self,
        stream_identifier: StreamId,
        last_ssn: u16,
        new_cumulative_tsn: u32,
    ) {
        if !self.forward_tsn_applies_to_current_generation(stream_identifier) {
            let generation_boundary = self.pending_reset_boundary(stream_identifier);
            let kind = {
                let updates = self
                    .deferred_forward_tsns
                    .entry(stream_identifier)
                    .or_default();
                let previous = updates.iter_mut().find(|update| {
                    if update.generation_boundary != generation_boundary {
                        return false;
                    }
                    let DeferredForwardTsnKind::Ordered {
                        last_ssn: previous_ssn,
                        ..
                    } = update.kind
                    else {
                        return false;
                    };
                    previous_ssn == last_ssn
                        || (generation_boundary.is_some() && sna16lt(previous_ssn, last_ssn))
                });

                if let Some(previous) = previous {
                    let DeferredForwardTsnKind::Ordered {
                        last_ssn: previous_ssn,
                        new_cumulative_tsn: previous_cumulative_tsn,
                    } = &mut previous.kind
                    else {
                        unreachable!();
                    };
                    if sna16lt(*previous_ssn, last_ssn) {
                        *previous_ssn = last_ssn;
                    }
                    if sna32lt(*previous_cumulative_tsn, new_cumulative_tsn) {
                        *previous_cumulative_tsn = new_cumulative_tsn;
                    }
                    previous.kind
                } else {
                    let kind = DeferredForwardTsnKind::Ordered {
                        last_ssn,
                        new_cumulative_tsn,
                    };
                    updates.push_back(DeferredForwardTsn {
                        generation_boundary,
                        kind,
                    });
                    kind
                }
            };
            self.prune_deferred_reset_data_for_forward_tsn(
                stream_identifier,
                generation_boundary,
                kind,
            );
            return;
        }

        let became_readable = self
            .streams
            .get_mut(&stream_identifier)
            .is_some_and(|stream| {
                let was_readable = stream.reassembly_queue.is_readable();
                stream.reassembly_queue.forward_tsn_for_ordered(last_ssn);
                !was_readable && stream.reassembly_queue.is_readable()
            });
        if became_readable {
            self.events.push_back(Event::Stream(StreamEvent::Readable {
                id: stream_identifier,
            }));
        }
    }

    fn apply_or_defer_unordered_forward_tsn(
        &mut self,
        stream_identifier: StreamId,
        new_cumulative_tsn: u32,
    ) {
        if !self.forward_tsn_applies_to_current_generation(stream_identifier) {
            let generation_boundary = self.pending_reset_boundary(stream_identifier);
            let effective_cumulative_tsn = {
                let updates = self
                    .deferred_forward_tsns
                    .entry(stream_identifier)
                    .or_default();
                if let Some(previous_cumulative_tsn) = updates.iter_mut().find_map(|update| {
                    if update.generation_boundary != generation_boundary {
                        return None;
                    }
                    match &mut update.kind {
                        DeferredForwardTsnKind::Unordered { new_cumulative_tsn } => {
                            Some(new_cumulative_tsn)
                        }
                        DeferredForwardTsnKind::Ordered { .. } => None,
                    }
                }) {
                    if sna32lt(*previous_cumulative_tsn, new_cumulative_tsn) {
                        *previous_cumulative_tsn = new_cumulative_tsn;
                    }
                    *previous_cumulative_tsn
                } else {
                    updates.push_back(DeferredForwardTsn {
                        generation_boundary,
                        kind: DeferredForwardTsnKind::Unordered { new_cumulative_tsn },
                    });
                    new_cumulative_tsn
                }
            };
            self.prune_deferred_reset_data_for_forward_tsn(
                stream_identifier,
                generation_boundary,
                DeferredForwardTsnKind::Unordered {
                    new_cumulative_tsn: effective_cumulative_tsn,
                },
            );
            return;
        }

        if let Some(stream) = self.streams.get_mut(&stream_identifier) {
            stream
                .reassembly_queue
                .forward_tsn_for_unordered(new_cumulative_tsn);
        }
    }

    fn apply_unordered_forward_tsn_to_current_streams(&mut self, new_cumulative_tsn: u32) {
        let stream_ids: Vec<StreamId> = self.streams.keys().copied().collect();
        for stream_identifier in stream_ids {
            self.apply_or_defer_unordered_forward_tsn(stream_identifier, new_cumulative_tsn);
        }
    }

    fn deferred_forward_tsn_application(
        &self,
        stream_identifier: StreamId,
        update: DeferredForwardTsn,
    ) -> Option<(DeferredForwardTsnKind, bool)> {
        match update.kind {
            DeferredForwardTsnKind::Ordered {
                last_ssn,
                new_cumulative_tsn,
            } => self.streams.get(&stream_identifier).and_then(|stream| {
                stream
                    .reassembly_queue
                    .applicable_forward_tsn_for_ordered_bounded(
                        last_ssn,
                        new_cumulative_tsn,
                        self.peer_last_tsn,
                    )
                    .map(|applicable_last_ssn| {
                        (
                            DeferredForwardTsnKind::Ordered {
                                last_ssn: applicable_last_ssn,
                                new_cumulative_tsn,
                            },
                            applicable_last_ssn == last_ssn,
                        )
                    })
            }),
            DeferredForwardTsnKind::Unordered { .. } => Some((update.kind, true)),
        }
    }

    fn apply_deferred_forward_tsns(&mut self, stream_identifier: StreamId) {
        if !self.streams.contains_key(&stream_identifier) {
            return;
        }

        let boundary = self.current_reset_boundary(stream_identifier);
        let Some(updates) = self.deferred_forward_tsns.get(&stream_identifier) else {
            return;
        };
        let mut removals = Vec::with_capacity(updates.len());
        let mut ready = vec![];
        for update in updates {
            let application = if update.generation_boundary == boundary {
                self.deferred_forward_tsn_application(stream_identifier, *update)
            } else {
                None
            };
            let remove = application.is_some_and(|(_, remove)| remove);
            removals.push(remove);
            if let Some((kind, _)) = application {
                ready.push(kind);
            }
        }

        if ready.is_empty() {
            return;
        }

        let mut index = 0;
        if let Some(updates) = self.deferred_forward_tsns.get_mut(&stream_identifier) {
            updates.retain(|_| {
                let retain = !removals[index];
                index += 1;
                retain
            });
        }
        if self
            .deferred_forward_tsns
            .get(&stream_identifier)
            .is_some_and(VecDeque::is_empty)
        {
            self.deferred_forward_tsns.remove(&stream_identifier);
        }

        let became_readable = if let Some(stream) = self.streams.get_mut(&stream_identifier) {
            let was_readable = stream.reassembly_queue.is_readable();
            for kind in ready {
                match kind {
                    DeferredForwardTsnKind::Ordered {
                        last_ssn,
                        new_cumulative_tsn,
                    } => {
                        stream
                            .reassembly_queue
                            .forward_tsn_for_ordered_bounded(last_ssn, new_cumulative_tsn);
                    }
                    DeferredForwardTsnKind::Unordered { new_cumulative_tsn } => {
                        stream
                            .reassembly_queue
                            .forward_tsn_for_unordered(new_cumulative_tsn);
                    }
                }
            }
            !was_readable && stream.reassembly_queue.is_readable()
        } else {
            false
        };
        if became_readable {
            self.events.push_back(Event::Stream(StreamEvent::Readable {
                id: stream_identifier,
            }));
        }
    }

    fn discard_deferred_forward_tsns_through(
        &mut self,
        stream_identifier: StreamId,
        boundary: u32,
    ) {
        if let Some(updates) = self.deferred_forward_tsns.get_mut(&stream_identifier) {
            updates.retain(|update| update.generation_boundary != Some(boundary));
            if updates.is_empty() {
                self.deferred_forward_tsns.remove(&stream_identifier);
            }
        }
    }

    /// Release receive-window credit for incomplete successor messages that a
    /// deferred Forward-TSN has abandoned. Complete messages remain available
    /// when that stream generation is eventually exposed to the application.
    fn prune_deferred_reset_data_for_forward_tsn(
        &mut self,
        stream_identifier: StreamId,
        generation_boundary: Option<u32>,
        kind: DeferredForwardTsnKind,
    ) {
        let mut chunks: Vec<ChunkPayloadData> = self
            .deferred_reset_data
            .values()
            .filter(|chunk| {
                chunk.stream_identifier == stream_identifier
                    && self
                        .deferred_generation_bounds(stream_identifier, chunk.tsn)
                        .is_some_and(|(_, upper)| upper == generation_boundary)
            })
            .cloned()
            .collect();
        if chunks.is_empty() {
            return;
        }

        chunks.sort_unstable_by_key(|chunk| {
            self.deferred_generation_bounds(stream_identifier, chunk.tsn)
                .map(|(lower, _)| chunk.tsn.wrapping_sub(lower))
                .unwrap_or_default()
        });
        let targeted_tsns: FxHashSet<u32> = chunks.iter().map(|chunk| chunk.tsn).collect();

        let mut queue = ReassemblyQueue::new(stream_identifier, self.max_receive_message_size);
        for chunk in chunks {
            // These chunks were validated before being retained. If rebuilding
            // their generation unexpectedly fails, keep the data conservatively.
            if queue.push(chunk).is_err() {
                return;
            }
        }
        match kind {
            DeferredForwardTsnKind::Ordered {
                last_ssn,
                new_cumulative_tsn,
            } => {
                queue.forward_tsn_for_ordered_bounded(last_ssn, new_cumulative_tsn);
            }
            DeferredForwardTsnKind::Unordered { new_cumulative_tsn } => {
                queue.forward_tsn_for_unordered(new_cumulative_tsn);
            }
        }

        let retained_tsns: FxHashSet<u32> = queue
            .ordered
            .iter()
            .chain(&queue.unordered)
            .flat_map(|chunks| chunks.chunks.iter())
            .chain(queue.unordered_chunks.iter())
            .map(|chunk| chunk.tsn)
            .collect();
        self.deferred_reset_data
            .retain(|tsn, _| !targeted_tsns.contains(tsn) || retained_tsns.contains(tsn));
    }

    fn deliver_deferred_reset_data(&mut self, d: &ChunkPayloadData) -> Result<()> {
        if self.get_or_create_stream(d.stream_identifier).is_none() {
            debug!("[{}] discard {}", self.side, d.stream_sequence_number);
            return Ok(());
        }

        if let Some(stream) = self.streams.get_mut(&d.stream_identifier) {
            let queued = stream.handle_data(d)?;
            self.events.push_back(Event::DatagramReceived);
            if queued && stream.reassembly_queue.is_readable() {
                self.events.push_back(Event::Stream(StreamEvent::Readable {
                    id: d.stream_identifier,
                }));
            }
        }

        Ok(())
    }

    fn release_deferred_generation_data(
        &mut self,
        stream_identifier: StreamId,
        previous_boundary: u32,
        boundary: u32,
    ) -> Result<()> {
        let mut ready: Vec<ChunkPayloadData> = self
            .deferred_reset_data
            .values()
            .filter(|chunk| {
                chunk.stream_identifier == stream_identifier
                    && sna32gt(chunk.tsn, previous_boundary)
                    && sna32lte(chunk.tsn, boundary)
            })
            .cloned()
            .collect();
        ready.sort_unstable_by_key(|chunk| chunk.tsn.wrapping_sub(previous_boundary));

        for chunk in ready {
            self.deliver_deferred_reset_data(&chunk)?;
            self.deferred_reset_data.remove(&chunk.tsn);
        }
        self.apply_deferred_forward_tsns(stream_identifier);

        Ok(())
    }

    fn deferred_generation_bounds(
        &self,
        stream_identifier: StreamId,
        tsn: u32,
    ) -> Option<(u32, Option<u32>)> {
        let retirement_boundaries = self.retiring_streams.get(&stream_identifier);
        let pending_boundary = self
            .reconfig_requests
            .values()
            .find(|request| Self::reset_request_affects_stream(request, stream_identifier))
            .map(|request| request.sender_last_tsn);

        let mut boundaries: Vec<u32> = retirement_boundaries
            .into_iter()
            .flat_map(|boundaries| boundaries.iter().copied())
            .collect();
        if let Some(pending_boundary) = pending_boundary {
            if boundaries.last().copied() != Some(pending_boundary) {
                boundaries.push(pending_boundary);
            }
        }

        let mut lower = *boundaries.first()?;
        for upper in boundaries.into_iter().skip(1) {
            if sna32lte(tsn, upper) {
                return Some((lower, Some(upper)));
            }
            lower = upper;
        }
        Some((lower, None))
    }

    fn validate_deferred_reset_data(&self, d: &ChunkPayloadData) -> Result<()> {
        let Some((lower, upper)) = self.deferred_generation_bounds(d.stream_identifier, d.tsn)
        else {
            return Ok(());
        };

        let mut chunks: Vec<ChunkPayloadData> = self
            .deferred_reset_data
            .values()
            .filter(|chunk| {
                chunk.stream_identifier == d.stream_identifier
                    && sna32gt(chunk.tsn, lower)
                    && upper.is_none_or(|upper| sna32lte(chunk.tsn, upper))
            })
            .cloned()
            .collect();
        chunks.sort_unstable_by_key(|chunk| chunk.tsn.wrapping_sub(lower));

        let mut validation_queue =
            ReassemblyQueue::new(d.stream_identifier, self.max_receive_message_size);
        for chunk in chunks {
            validation_queue.push(chunk)?;
        }
        validation_queue.push(d.clone())?;
        Ok(())
    }

    fn release_deferred_reset_data(&mut self) -> Result<()> {
        let ready: Vec<ChunkPayloadData> = self
            .deferred_reset_data
            .values()
            .filter(|chunk| !self.data_is_above_pending_reset(chunk))
            .cloned()
            .collect();
        let stream_ids: FxHashSet<StreamId> =
            ready.iter().map(|chunk| chunk.stream_identifier).collect();

        for chunk in ready {
            self.deliver_deferred_reset_data(&chunk)?;
            self.deferred_reset_data.remove(&chunk.tsn);
        }
        for stream_identifier in stream_ids {
            self.apply_deferred_forward_tsns(stream_identifier);
        }

        Ok(())
    }

    fn handle_data(&mut self, d: &ChunkPayloadData) -> Result<Vec<Packet>> {
        trace!(
            "[{}] DATA: tsn={} immediateSack={} len={}",
            self.side,
            d.tsn,
            d.immediate_sack,
            d.user_data.len()
        );
        self.stats.inc_datas();

        let can_push = self.payload_queue.can_push(d, self.peer_last_tsn);
        let mut stream_handle_data = false;
        let mut defer_stream_data = false;
        if can_push && self.data_is_above_pending_reset(d) {
            if self.get_my_receiver_window_credit() > 0 {
                defer_stream_data = true;
            } else if let Some(last_tsn) = self.payload_queue.get_last_tsn_received() {
                if sna32lt(d.tsn, *last_tsn) {
                    debug!(
                        "[{}] receive buffer full, but accepted deferred \
                        reset DATA as missing chunk tsn={} ssn={}",
                        self.side, d.tsn, d.stream_sequence_number
                    );
                    defer_stream_data = true;
                }
            }
        } else if can_push {
            if self.get_or_create_stream(d.stream_identifier).is_some() {
                if self.get_my_receiver_window_credit() > 0 {
                    // Pass the new chunk to stream level as soon as it arrives
                    stream_handle_data = true;
                } else {
                    // Receive buffer is full
                    if let Some(last_tsn) = self.payload_queue.get_last_tsn_received() {
                        if sna32lt(d.tsn, *last_tsn) {
                            debug!(
                                "[{}] receive buffer full, but accepted \
                                as missing chunk tsn={} ssn={}",
                                self.side, d.tsn, d.stream_sequence_number
                            );
                            stream_handle_data = true;
                        }
                    } else {
                        debug!(
                            "[{}] receive buffer full. dropping DATA with tsn={} ssn={}",
                            self.side, d.tsn, d.stream_sequence_number
                        );
                    }
                }
            } else {
                // silently discard the data. (sender will retry on T3-rtx timeout)
                // see pion/sctp#30
                debug!("[{}] discard {}", self.side, d.stream_sequence_number);
                return Ok(vec![]);
            }
        }

        let immediate_sack = d.immediate_sack;

        if defer_stream_data {
            self.validate_deferred_reset_data(d)?;
            if self.payload_queue.push(d.clone(), self.peer_last_tsn) {
                self.deferred_reset_data.insert(d.tsn, d.clone());
            }
        } else if stream_handle_data {
            if let Some(s) = self.streams.get_mut(&d.stream_identifier) {
                let queued = s.handle_data(d)?;
                // Only commit to payload_queue after reassembly accepts the chunk
                self.payload_queue.push(d.clone(), self.peer_last_tsn);
                self.events.push_back(Event::DatagramReceived);
                if queued && s.reassembly_queue.is_readable() {
                    self.events.push_back(Event::Stream(StreamEvent::Readable {
                        id: d.stream_identifier,
                    }))
                }
            }
            // A deferred skip may describe this successor generation. Queue
            // DATA first so a complete message reusing the skipped SSN remains
            // readable, while a genuinely abandoned SSN can still be advanced.
            self.apply_deferred_forward_tsns(d.stream_identifier);
        }

        self.handle_peer_last_tsn_and_acknowledgement(immediate_sack)
    }

    fn handle_sack(&mut self, d: &ChunkSelectiveAck, now: Instant) -> Result<Vec<Packet>> {
        trace!(
            "[{}] {}, SACK: cumTSN={} a_rwnd={}",
            self.side,
            self.cumulative_tsn_ack_point,
            d.cumulative_tsn_ack,
            d.advertised_receiver_window_credit
        );
        let state = self.state();
        if state != AssociationState::Established
            && state != AssociationState::ShutdownPending
            && state != AssociationState::ShutdownReceived
        {
            return Ok(vec![]);
        }

        self.stats.inc_sacks();

        if sna32gt(self.cumulative_tsn_ack_point, d.cumulative_tsn_ack) {
            // RFC 4960 sec 6.2.1.  Processing a Received SACK
            // D)
            //   i) If Cumulative TSN Ack is less than the Cumulative TSN Ack
            //      Point, then drop the SACK.  Since Cumulative TSN Ack is
            //      monotonically increasing, a SACK whose Cumulative TSN Ack is
            //      less than the Cumulative TSN Ack Point indicates an out-of-
            //      order SACK.

            debug!(
                "[{}] SACK Cumulative ACK {} is older than ACK point {}",
                self.side, d.cumulative_tsn_ack, self.cumulative_tsn_ack_point
            );

            return Ok(vec![]);
        }

        // Process selective ack
        let outstanding_before = self.outbound_queue.inflight_bytes();
        let (bytes_acked_per_stream, htna) = self.process_selective_ack(d, now)?;
        // Peer delivery earns congestion credit even after its stream was reset.
        // Application credit below belongs only to the current stream generation.
        let total_bytes_acked = (outstanding_before - self.outbound_queue.inflight_bytes()) as i64;

        let mut cum_tsn_ack_point_advanced = false;
        if sna32lt(self.cumulative_tsn_ack_point, d.cumulative_tsn_ack) {
            trace!(
                "[{}] SACK: cumTSN advanced: {} -> {}",
                self.side, self.cumulative_tsn_ack_point, d.cumulative_tsn_ack
            );

            self.cumulative_tsn_ack_point = d.cumulative_tsn_ack;
            cum_tsn_ack_point_advanced = true;
            self.on_cumulative_tsn_ack_point_advanced(total_bytes_acked, now);
        }

        for (si, n_bytes_acked) in &bytes_acked_per_stream {
            if let Some(s) = self.streams.get_mut(si) {
                if s.on_buffer_released(*n_bytes_acked) {
                    self.events
                        .push_back(Event::Stream(StreamEvent::BufferedAmountLow { id: *si }))
                }
            }
        }

        // New rwnd value
        // RFC 4960 sec 6.2.1.  Processing a Received SACK
        // D)
        //   ii) Set rwnd equal to the newly received a_rwnd minus the number
        //       of bytes still outstanding after processing the Cumulative
        //       TSN Ack and the Gap Ack Blocks.

        // bytes acked were already subtracted by markAsAcked() method
        let bytes_outstanding = self.outbound_queue.inflight_bytes() as u32;
        if bytes_outstanding >= d.advertised_receiver_window_credit {
            self.rwnd = 0;
        } else {
            self.rwnd = d.advertised_receiver_window_credit - bytes_outstanding;
        }

        self.process_fast_retransmission(d.cumulative_tsn_ack, htna, cum_tsn_ack_point_advanced)?;

        if self.use_forward_tsn {
            self.advance_peer_tsn_ack_point();

            // RFC 3758 Sec 3.5 C3
            if sna32gt(
                self.advanced_peer_tsn_ack_point,
                self.cumulative_tsn_ack_point,
            ) {
                self.will_send_forward_tsn = true;
                debug!(
                    "[{}] handleSack {}: sna32GT({}, {})",
                    self.side,
                    self.will_send_forward_tsn,
                    self.advanced_peer_tsn_ack_point,
                    self.cumulative_tsn_ack_point
                );
            }
            self.awake_write_loop();
        }

        self.postprocess_sack(state, cum_tsn_ack_point_advanced, now);

        Ok(vec![])
    }

    fn handle_reconfig(&mut self, c: &ChunkReconfig) -> Result<Vec<Packet>> {
        trace!("[{}] handle_reconfig", self.side);

        let mut pp = vec![];

        if let Some(param_a) = &c.param_a {
            self.handle_reconfig_param(param_a, &mut pp)?;
        }

        if let Some(param_b) = &c.param_b {
            self.handle_reconfig_param(param_b, &mut pp)?;
        }

        Ok(pp)
    }

    fn handle_forward_tsn(&mut self, c: &ChunkForwardTsn) -> Result<Vec<Packet>> {
        trace!("[{}] FwdTSN: {}", self.side, c);

        if !self.use_forward_tsn {
            warn!("[{}] received FwdTSN but not enabled", self.side);
            // Return an error chunk
            let cerr = ChunkError {
                error_causes: vec![ErrorCauseUnrecognizedChunkType::default()],
            };

            let outbound = Packet {
                common_header: CommonHeader {
                    verification_tag: self.peer_verification_tag,
                    source_port: self.source_port,
                    destination_port: self.destination_port,
                },
                chunks: vec![Box::new(cerr)],
            };
            return Ok(vec![outbound]);
        }

        // From RFC 3758 Sec 3.6:
        //   Note, if the "New Cumulative TSN" value carried in the arrived
        //   FORWARD TSN chunk is found to be behind or at the current cumulative
        //   TSN point, the data receiver MUST treat this FORWARD TSN as out-of-
        //   date and MUST NOT update its Cumulative TSN.  The receiver SHOULD
        //   send a SACK to its peer (the sender of the FORWARD TSN) since such a
        //   duplicate may indicate the previous SACK was lost in the network.

        trace!(
            "[{}] should send ack? newCumTSN={} peer_last_tsn={}",
            self.side, c.new_cumulative_tsn, self.peer_last_tsn
        );
        if sna32lte(c.new_cumulative_tsn, self.peer_last_tsn) {
            trace!("[{}] sending ack on Forward TSN", self.side);
            self.ack_state = AckState::Immediate;
            self.timers.stop(Timer::Ack);
            self.awake_write_loop();
            return Ok(vec![]);
        }

        // From RFC 3758 Sec 3.6:
        //   the receiver MUST perform the same TSN handling, including duplicate
        //   detection, gap detection, SACK generation, cumulative TSN
        //   advancement, etc. as defined in RFC 2960 [2]---with the following
        //   exceptions and additions.

        //   When a FORWARD TSN chunk arrives, the data receiver MUST first update
        //   its cumulative TSN point to the value carried in the FORWARD TSN
        //   chunk,

        // Advance to the peer's new cumulative tsn point,
        // dropping the chunks it abandoned along the way.
        self.payload_queue.pop_up_to(c.new_cumulative_tsn);
        self.peer_last_tsn = c.new_cumulative_tsn;

        // Report new peer_last_tsn value and abandoned largest SSN value to
        // corresponding streams so that the abandoned chunks can be removed
        // from the reassemblyQueue.
        for forwarded in &c.streams {
            self.apply_or_defer_ordered_forward_tsn(
                forwarded.identifier,
                forwarded.sequence,
                c.new_cumulative_tsn,
            );
        }

        // TSN may be forwarded for unordered chunks. ForwardTSN chunk does not
        // report which stream identifier it skipped for unordered chunks.
        // Therefore, we need to broadcast this event to all existing streams for
        // unordered chunks.
        // See https://github.com/pion/sctp/issues/106
        self.apply_unordered_forward_tsn_to_current_streams(c.new_cumulative_tsn);

        let mut reply = vec![];
        self.reevaluate_pending_reset_requests(&mut reply)?;
        reply.extend(self.handle_peer_last_tsn_and_acknowledgement(false)?);
        Ok(reply)
    }

    /// Handle I-FORWARD-TSN (RFC 8260) — identical to FORWARD-TSN but with
    /// 32-bit MID and explicit per-entry unordered flag.
    fn handle_i_forward_tsn(&mut self, c: &ChunkIForwardTsn) -> Result<Vec<Packet>> {
        trace!("[{}] I-FwdTSN: {}", self.side, c);

        if !self.use_forward_tsn {
            warn!("[{}] received I-FwdTSN but not enabled", self.side);
            let cerr = ChunkError {
                error_causes: vec![ErrorCauseUnrecognizedChunkType::default()],
            };

            let outbound = Packet {
                common_header: CommonHeader {
                    verification_tag: self.peer_verification_tag,
                    source_port: self.source_port,
                    destination_port: self.destination_port,
                },
                chunks: vec![Box::new(cerr)],
            };
            return Ok(vec![outbound]);
        }

        if sna32lte(c.new_cumulative_tsn, self.peer_last_tsn) {
            trace!("[{}] sending ack on I-Forward TSN", self.side);
            self.ack_state = AckState::Immediate;
            self.timers.stop(Timer::Ack);
            self.awake_write_loop();
            return Ok(vec![]);
        }

        // Advance to the peer's new cumulative tsn point,
        // dropping the chunks it abandoned along the way.
        self.payload_queue.pop_up_to(c.new_cumulative_tsn);
        self.peer_last_tsn = c.new_cumulative_tsn;

        // Handle per-stream entries using the explicit unordered flag
        for forwarded in &c.streams {
            if forwarded.unordered {
                self.apply_or_defer_unordered_forward_tsn(
                    forwarded.identifier,
                    c.new_cumulative_tsn,
                );
            } else {
                // MID maps to SSN for ordered streams; truncate to u16
                self.apply_or_defer_ordered_forward_tsn(
                    forwarded.identifier,
                    forwarded.mid as u16,
                    c.new_cumulative_tsn,
                );
            }
        }

        // Broadcast to all unordered streams
        self.apply_unordered_forward_tsn_to_current_streams(c.new_cumulative_tsn);

        let mut reply = vec![];
        self.reevaluate_pending_reset_requests(&mut reply)?;
        reply.extend(self.handle_peer_last_tsn_and_acknowledgement(false)?);
        Ok(reply)
    }

    fn handle_shutdown(&mut self, _: &ChunkShutdown) -> Result<Vec<Packet>> {
        let state = self.state();

        if state == AssociationState::Established {
            if self.outbound_queue.inflight_len() != 0 {
                self.set_state(AssociationState::ShutdownReceived);
            } else {
                // No more outstanding, send shutdown ack.
                self.will_send_shutdown_ack = true;
                self.set_state(AssociationState::ShutdownAckSent);

                self.awake_write_loop();
            }
        } else if state == AssociationState::ShutdownSent {
            // self.cumulative_tsn_ack_point = c.cumulative_tsn_ack

            self.will_send_shutdown_ack = true;
            self.set_state(AssociationState::ShutdownAckSent);

            self.awake_write_loop();
        }

        Ok(vec![])
    }

    fn handle_shutdown_ack(&mut self, _: &ChunkShutdownAck) -> Result<Vec<Packet>> {
        let state = self.state();
        if state == AssociationState::ShutdownSent || state == AssociationState::ShutdownAckSent {
            self.timers.stop(Timer::T2Shutdown);
            self.will_send_shutdown_complete = true;

            self.awake_write_loop();
        }

        Ok(vec![])
    }

    fn handle_shutdown_complete(&mut self, _: &ChunkShutdownComplete) -> Result<Vec<Packet>> {
        let state = self.state();
        if state == AssociationState::ShutdownAckSent {
            self.timers.stop(Timer::T2Shutdown);
            self.close()?;
        }

        Ok(vec![])
    }

    fn reevaluate_pending_reset_requests(&mut self, reply: &mut Vec<Packet>) -> Result<()> {
        let requests: Vec<ParamOutgoingResetRequest> =
            self.reconfig_requests.values().cloned().collect();
        for request in requests {
            let seq = request.reconfig_request_sequence_number;
            self.reset_streams_if_any(&request, false, reply)?;
            if !self.reconfig_requests.contains_key(&seq) {
                self.max_completed_reconfig_rsn = Some(seq);
            }
        }
        Ok(())
    }

    /// A common routine for handle_data and handle_forward_tsn routines
    fn handle_peer_last_tsn_and_acknowledgement(
        &mut self,
        sack_immediately: bool,
    ) -> Result<Vec<Packet>> {
        let mut reply = vec![];

        // Try to advance peer_last_tsn

        // From RFC 3758 Sec 3.6:
        //   .. and then MUST further advance its cumulative TSN point locally
        //   if possible
        // Meaning, if peer_last_tsn+1 points to a chunk that is received,
        // advance peer_last_tsn until peer_last_tsn+1 points to unreceived chunk.
        //debug!("[{}] peer_last_tsn = {}", self.side, self.peer_last_tsn);
        while let Some(chunk) = self.payload_queue.pop(self.peer_last_tsn.wrapping_add(1)) {
            self.peer_last_tsn = self.peer_last_tsn.wrapping_add(1);
            //debug!("[{}] peer_last_tsn = {}", self.side, self.peer_last_tsn);

            self.reevaluate_pending_reset_requests(&mut reply)?;

            if let Some(deferred) = self.deferred_reset_data.get(&chunk.tsn).cloned() {
                if !self.data_is_above_pending_reset(&deferred) {
                    self.deliver_deferred_reset_data(&deferred)?;
                    self.deferred_reset_data.remove(&chunk.tsn);
                }
            }
        }

        // A resolved TSN gap can disambiguate a deferred ordered skip whose
        // later SSN arrived first on a reset successor.
        let stream_ids: Vec<StreamId> = self.deferred_forward_tsns.keys().copied().collect();
        for stream_identifier in stream_ids {
            self.apply_deferred_forward_tsns(stream_identifier);
        }

        // DATA and FORWARD-TSN can both close the final gap. Once every
        // accepted TSN is cumulative, only new highest-TSN DATA could complete
        // an unreadable message; RFC 9260 section 6.2 forbids admitting it at
        // zero credit. Fail explicitly rather than pinning receive storage.
        if self.receive_capacity_exhausted() {
            return Err(Error::ErrReceiveBufferExhausted);
        }

        let has_packet_loss = !self.payload_queue.is_empty();
        if has_packet_loss {
            trace!(
                "[{}] packetloss: {}",
                self.side,
                self.payload_queue
                    .get_gap_ack_blocks_string(self.peer_last_tsn)
            );
        }

        if (self.ack_state != AckState::Immediate
            && !sack_immediately
            && !has_packet_loss
            && self.ack_mode == AckMode::Normal)
            || self.ack_mode == AckMode::AlwaysDelay
        {
            if self.ack_state == AckState::Idle {
                self.delayed_ack_triggered = true;
            } else {
                self.immediate_ack_triggered = true;
            }
        } else {
            self.immediate_ack_triggered = true;
        }

        Ok(reply)
    }

    #[allow(clippy::borrowed_box)]
    fn handle_reconfig_param(
        &mut self,
        raw: &Box<dyn Param + Send + Sync>,
        reply: &mut Vec<Packet>,
    ) -> Result<()> {
        if let Some(p) = raw.as_any().downcast_ref::<ParamOutgoingResetRequest>() {
            // RFC 6525 section 5.2.2 E1: the response sequence number in an
            // Outgoing Reset Request implicitly acknowledges our request.
            self.finish_reconfig(p.reconfig_response_sequence_number, Ok(()));

            let seq = p.reconfig_request_sequence_number;
            // Detect retransmission of a completed request. An InProgress request
            // is still in reconfig_requests, so we must let those through for
            // re-evaluation (the TSN may have advanced).
            if !self.reconfig_requests.contains_key(&seq)
                && self
                    .max_completed_reconfig_rsn
                    .is_some_and(|w| sna32lte(seq, w))
            {
                // Retransmission of an already-completed request. Resend the response
                // but do NOT reprocess stream resets (stream IDs may have been reused).
                self.push_reconfig_response(reply, seq, ReconfigResult::SuccessPerformed);
                return Ok(());
            }

            if !self.reconfig_requests.contains_key(&seq) {
                // RFC 6525 section 5.2.2 E4: only the next expected peer
                // request sequence number may start a new operation. A request
                // already deferred in reconfig_requests is a retransmission and
                // remains eligible for re-evaluation.
                if !self.reconfig_requests.is_empty() {
                    self.push_reconfig_response(
                        reply,
                        seq,
                        ReconfigResult::ErrorRequestAlreadyInProgress,
                    );
                    return Ok(());
                }

                if self.peer_reconfig_rsn_initialized
                    && seq != self.peer_last_reconfig_rsn.wrapping_add(1)
                {
                    self.push_reconfig_response(reply, seq, ReconfigResult::ErrorBadSequenceNumber);
                    return Ok(());
                }

                self.peer_last_reconfig_rsn = seq;
                self.peer_reconfig_rsn_initialized = true;
                self.reconfig_requests.insert(seq, p.clone());
            }

            // Re-evaluate the original request for this RSN. This prevents a
            // retransmission with altered stream identifiers or TSN boundary
            // from changing an operation that is already in progress.
            let request = self.reconfig_requests.get(&seq).cloned().unwrap();
            self.reset_streams_if_any(&request, true, reply)?;
            // Update watermark only after successful completion (request
            // removed from reconfig_requests by reset_streams_if_any).
            if !self.reconfig_requests.contains_key(&seq) {
                self.max_completed_reconfig_rsn = Some(seq);
            }
            Ok(())
        } else if let Some(p) = raw.as_any().downcast_ref::<ParamReconfigResponse>() {
            let rsn = p.reconfig_response_sequence_number;
            // RFC 6525 section 5.2.7 H1: ignore responses unless this RSN owns
            // the running Re-configuration Timer.
            if self.active_reconfig != Some(rsn) {
                return Ok(());
            }

            // In progress result means the peer has deferred the request,
            // not answered it. The request stays outstanding and its timer restarts
            // without counting toward the retransmission limit.
            if p.result == ReconfigResult::InProgress {
                if self.reconfigs.contains_key(&rsn) {
                    self.will_retransmit_reconfig = false;
                    // Pause without resetting the existing retry count. The outbound
                    // poll starts it again and the next expiry is exempted by H2.
                    self.timers.set(Timer::Reconfig, None);
                    self.timers.suppress_error_count(Timer::Reconfig);
                    self.awake_write_loop();
                }
                return Ok(());
            }

            let outcome = match p.result {
                ReconfigResult::SuccessNop | ReconfigResult::SuccessPerformed => Ok(()),
                ReconfigResult::Denied => Err(StreamResetError::Denied),
                _ => Err(StreamResetError::Failed),
            };
            self.finish_reconfig(rsn, outcome);
            Ok(())
        } else {
            Err(Error::ErrParameterType)
        }
    }

    fn push_reconfig_response(
        &self,
        reply: &mut Vec<Packet>,
        sequence_number: u32,
        result: ReconfigResult,
    ) {
        reply.push(self.create_packet(vec![Box::new(ChunkReconfig {
            param_a: Some(Box::new(ParamReconfigResponse {
                reconfig_response_sequence_number: sequence_number,
                result,
            })),
            param_b: None,
        })]));
    }

    fn process_selective_ack(
        &mut self,
        d: &ChunkSelectiveAck,
        now: Instant,
    ) -> Result<(HashMap<u16, i64>, u32)> {
        let mut bytes_acked_per_stream = HashMap::new();
        let mut newly_acked_data = false;
        let earliest_outstanding = self.outbound_queue.earliest_outstanding_tsn();

        // New ack point, so pop cumulatively ACKed chunks from the outbound queue.
        // We add 1 because the "currentAckPoint" has already been popped from the inflight queue
        // For the first SACK we take care of this by setting the ackpoint to cumAck - 1
        let first_outstanding_tsn = self.cumulative_tsn_ack_point.wrapping_add(1);
        let mut i = first_outstanding_tsn;
        //log::debug!("[{}] i={} d={}", self.name, i, d.cumulative_tsn_ack);
        while sna32lte(i, d.cumulative_tsn_ack) {
            if let Some(c) = self.outbound_queue.pop(i) {
                newly_acked_data |= !c.acked && c.nsent > 0;
                if !c.acked && !c.abandoned {
                    let n_bytes_acked = c.user_data.len() as i64;

                    // Sum the number of bytes acknowledged per stream
                    if !self
                        .streams
                        .get(&c.stream_identifier)
                        .is_some_and(|stream| c.message_id < stream.first_message_id)
                    {
                        *bytes_acked_per_stream
                            .entry(c.stream_identifier)
                            .or_insert(0) += n_bytes_acked;
                    }

                    // RFC 4960 sec 6.3.1.  RTO Calculation
                    //   C4)  When data is in flight and when allowed by rule C5 below, a new
                    //        RTT measurement MUST be made each round trip.  Furthermore, new
                    //        RTT measurements SHOULD be made no more than once per round trip
                    //        for a given destination transport address.
                    //   C5)  Karn's algorithm: RTT measurements MUST NOT be made using
                    //        packets that were retransmitted (and thus for which it is
                    //        ambiguous whether the reply was for the first instance of the
                    //        chunk or for a later instance)
                    if c.nsent == 1 && sna32gte(c.tsn, self.min_tsn2measure_rtt) {
                        self.min_tsn2measure_rtt = self.my_next_tsn;
                        if let Some(since) = &c.since {
                            let rtt = now.duration_since(*since);
                            let srtt = self.rto_mgr.set_new_rtt(rtt.as_millis() as u64);
                            trace!(
                                "[{}] SACK: measured-rtt={} srtt={} new-rto={}",
                                self.side,
                                rtt.as_millis(),
                                srtt,
                                self.rto_mgr.get_rto()
                            );
                        } else {
                            error!("[{}] invalid c.since", self.side);
                        }
                    }
                }

                if self.in_fast_recovery && c.tsn == self.fast_recover_exit_point {
                    debug!("[{}] exit fast-recovery", self.side);
                    self.in_fast_recovery = false;
                }
            } else {
                return Err(Error::ErrInflightQueueTsnPop);
            }

            i = i.wrapping_add(1);
        }

        let mut htna = d.cumulative_tsn_ack;

        // Mark selectively acknowledged chunks as "acked"
        for g in &d.gap_ack_blocks {
            for i in g.start..=g.end {
                let tsn = d.cumulative_tsn_ack.wrapping_add(i as u32);

                let (is_existed, is_acked) = if let Some(c) = self.outbound_queue.get(tsn) {
                    newly_acked_data |= !c.acked && c.nsent > 0;
                    (true, c.acked)
                } else {
                    (false, false)
                };
                let n_bytes_acked = if is_existed && !is_acked {
                    self.outbound_queue.mark_as_acked(tsn) as i64
                } else {
                    0
                };

                if let Some(c) = self.outbound_queue.get(tsn) {
                    if !is_acked && !c.abandoned {
                        // Sum the number of bytes acknowledged per stream
                        if !self
                            .streams
                            .get(&c.stream_identifier)
                            .is_some_and(|stream| c.message_id < stream.first_message_id)
                        {
                            *bytes_acked_per_stream
                                .entry(c.stream_identifier)
                                .or_insert(0) += n_bytes_acked;
                        }

                        trace!("[{}] tsn={} has been sacked", self.side, c.tsn);

                        if c.nsent == 1 {
                            self.min_tsn2measure_rtt = self.my_next_tsn;
                            if let Some(since) = &c.since {
                                let rtt = now.duration_since(*since);
                                let srtt = self.rto_mgr.set_new_rtt(rtt.as_millis() as u64);
                                trace!(
                                    "[{}] SACK: measured-rtt={} srtt={} new-rto={}",
                                    self.side,
                                    rtt.as_millis(),
                                    srtt,
                                    self.rto_mgr.get_rto()
                                );
                            } else {
                                error!("[{}] invalid c.since", self.side);
                            }
                        }

                        if sna32lt(htna, tsn) {
                            htna = tsn;
                        }
                    }
                } else {
                    return Err(Error::ErrTsnRequestNotExist);
                }
            }
        }

        // Local retirement and duplicate SACKs are not new peer progress.
        if newly_acked_data {
            self.timers.reset_retrans(Timer::T3RTX);
        }
        // RFC 4960 R3 applies to the earliest outstanding DATA, even when
        // abandoned TSNs precede it or it is acknowledged by a Gap ACK.
        if earliest_outstanding.is_some_and(|tsn| {
            sna32lte(tsn, d.cumulative_tsn_ack)
                || self.outbound_queue.get(tsn).is_some_and(|c| c.acked)
        }) {
            self.timers.set(Timer::T3RTX, None);
        }
        Ok((bytes_acked_per_stream, htna))
    }

    fn on_cumulative_tsn_ack_point_advanced(&mut self, total_bytes_acked: i64, now: Instant) {
        // RFC 4096, sec 6.3.2.  Retransmission Timer Rules
        //   R2)  Whenever all outstanding data sent to an address have been
        //        acknowledged, turn off the T3-rtx timer of that address.
        if self.outbound_queue.inflight_len() == 0 {
            trace!(
                "[{}] SACK: no more packet in-flight (pending={})",
                self.side,
                self.outbound_queue.pending_len()
            );
            self.timers.stop(Timer::T3RTX);
        } else {
            trace!("[{}] T3-rtx timer start (pt2)", self.side);
            self.timers
                .restart_if_stale(Timer::T3RTX, now, self.rto_mgr.get_rto());
        }

        // Update congestion control parameters
        if self.cwnd <= self.ssthresh {
            // RFC 4096, sec 7.2.1.  Slow-Start
            //   o  When cwnd is less than or equal to ssthresh, an SCTP endpoint MUST
            //		use the slow-start algorithm to increase cwnd only if the current
            //      congestion window is being fully utilized, an incoming SACK
            //      advances the Cumulative TSN Ack Point, and the data sender is not
            //      in Fast Recovery.  Only when these three conditions are met can
            //      the cwnd be increased; otherwise, the cwnd MUST not be increased.
            //		If these conditions are met, then cwnd MUST be increased by, at
            //      most, the lesser of 1) the total size of the previously
            //      outstanding DATA chunk(s) acknowledged, and 2) the destination's
            //      path MTU.
            if !self.in_fast_recovery && self.outbound_queue.pending_len() != 0 {
                self.cwnd += core::cmp::min(total_bytes_acked as u32, self.cwnd); // TCP way
                // self.cwnd += min32(uint32(total_bytes_acked), self.mtu) // SCTP way (slow)
                trace!(
                    "[{}] updated cwnd={} ssthresh={} acked={} (SS)",
                    self.side, self.cwnd, self.ssthresh, total_bytes_acked
                );
            } else {
                trace!(
                    "[{}] cwnd did not grow: cwnd={} ssthresh={} acked={} FR={} pending={}",
                    self.side,
                    self.cwnd,
                    self.ssthresh,
                    total_bytes_acked,
                    self.in_fast_recovery,
                    self.outbound_queue.pending_len()
                );
            }
        } else {
            // RFC 4096, sec 7.2.2.  Congestion Avoidance
            //   o  Whenever cwnd is greater than ssthresh, upon each SACK arrival
            //      that advances the Cumulative TSN Ack Point, increase
            //      partial_bytes_acked by the total number of bytes of all new chunks
            //      acknowledged in that SACK including chunks acknowledged by the new
            //      Cumulative TSN Ack and by Gap Ack Blocks.
            self.partial_bytes_acked += total_bytes_acked as u32;

            //   o  When partial_bytes_acked is equal to or greater than cwnd and
            //      before the arrival of the SACK the sender had cwnd or more bytes
            //      of data outstanding (i.e., before arrival of the SACK, flight size
            //      was greater than or equal to cwnd), increase cwnd by MTU, and
            //      reset partial_bytes_acked to (partial_bytes_acked - cwnd).
            if self.partial_bytes_acked >= self.cwnd && self.outbound_queue.pending_len() != 0 {
                self.partial_bytes_acked -= self.cwnd;
                self.cwnd += self.mtu;
                trace!(
                    "[{}] updated cwnd={} ssthresh={} acked={} (CA)",
                    self.side, self.cwnd, self.ssthresh, total_bytes_acked
                );
            }
        }
    }

    fn process_fast_retransmission(
        &mut self,
        cum_tsn_ack_point: u32,
        htna: u32,
        cum_tsn_ack_point_advanced: bool,
    ) -> Result<()> {
        // HTNA algorithm - RFC 4960 Sec 7.2.4
        // Increment missIndicator of each chunks that the SACK reported missing
        // when either of the following is met:
        // a)  Not in fast-recovery
        //     miss indications are incremented only for missing TSNs prior to the
        //     highest TSN newly acknowledged in the SACK.
        // b)  In fast-recovery AND the Cumulative TSN Ack Point advanced
        //     the miss indications are incremented for all TSNs reported missing
        //     in the SACK.
        if !self.in_fast_recovery || cum_tsn_ack_point_advanced {
            let max_tsn = if !self.in_fast_recovery {
                // a) increment only for missing TSNs prior to the HTNA
                htna
            } else {
                // b) increment for all TSNs reported missing
                cum_tsn_ack_point
                    .wrapping_add(self.outbound_queue.inflight_len() as u32)
                    .wrapping_add(1)
            };

            let mut tsn = cum_tsn_ack_point.wrapping_add(1);
            while sna32lt(tsn, max_tsn) {
                if let Some(c) = self.outbound_queue.get_mut(tsn) {
                    if !c.acked && !c.abandoned && c.miss_indicator < 3 {
                        c.miss_indicator += 1;
                        if c.miss_indicator == 3 && !self.in_fast_recovery {
                            // 2)  If not in Fast Recovery, adjust the ssthresh and cwnd of the
                            //     destination address(es) to which the missing DATA chunks were
                            //     last sent, according to the formula described in Section 7.2.3.
                            self.in_fast_recovery = true;
                            self.fast_recover_exit_point = htna;
                            self.ssthresh = core::cmp::max(self.cwnd / 2, 4 * self.mtu);
                            self.cwnd = self.ssthresh;
                            self.partial_bytes_acked = 0;
                            self.will_retransmit_fast = true;

                            trace!(
                                "[{}] updated cwnd={} ssthresh={} inflight={} (FR)",
                                self.side,
                                self.cwnd,
                                self.ssthresh,
                                self.outbound_queue.inflight_bytes()
                            );
                        }
                    }
                } else {
                    return Err(Error::ErrTsnRequestNotExist);
                }

                tsn = tsn.wrapping_add(1);
            }
        }

        if self.in_fast_recovery && cum_tsn_ack_point_advanced {
            self.will_retransmit_fast = true;
        }

        Ok(())
    }

    /// The caller must hold the lock. This method was only added because the
    /// linter was complaining about the "cognitive complexity" of handle_sack.
    fn postprocess_sack(
        &mut self,
        state: AssociationState,
        mut should_awake_write_loop: bool,
        now: Instant,
    ) {
        if self.outbound_queue.inflight_len() != 0 {
            // Start timer. (noop if already started)
            trace!("[{}] T3-rtx timer start (pt3)", self.side);
            self.timers
                .restart_if_stale(Timer::T3RTX, now, self.rto_mgr.get_rto());
        } else if state == AssociationState::ShutdownPending {
            // No more outstanding, send shutdown.
            should_awake_write_loop = true;
            self.will_send_shutdown = true;
            self.set_state(AssociationState::ShutdownSent);
        } else if state == AssociationState::ShutdownReceived {
            // No more outstanding, send shutdown ack.
            should_awake_write_loop = true;
            self.will_send_shutdown_ack = true;
            self.set_state(AssociationState::ShutdownAckSent);
        }

        if should_awake_write_loop {
            self.awake_write_loop();
        }
    }

    fn reset_streams_if_any(
        &mut self,
        p: &ParamOutgoingResetRequest,
        from_wire: bool,
        reply: &mut Vec<Packet>,
    ) -> Result<()> {
        let mut result = ReconfigResult::SuccessPerformed;
        let mut sis_to_reset = vec![];

        let performed = sna32lte(p.sender_last_tsn, self.peer_last_tsn);
        if performed {
            debug!(
                "[{}] resetStream(): senderLastTSN={} <= peer_last_tsn={}",
                self.side, p.sender_last_tsn, self.peer_last_tsn
            );
            let mut stream_ids = if p.stream_identifiers.is_empty() {
                self.streams.keys().copied().collect::<Vec<_>>()
            } else {
                p.stream_identifiers.clone()
            };
            stream_ids.sort_unstable();
            stream_ids.dedup();

            for id in stream_ids {
                if self.streams.contains_key(&id) {
                    sis_to_reset.push(id);
                    self.retire_stream(id, p.sender_last_tsn);
                }
            }
            self.reconfig_requests
                .remove(&p.reconfig_request_sequence_number);
        } else {
            debug!(
                "[{}] resetStream(): senderLastTSN={} > peer_last_tsn={}",
                self.side, p.sender_last_tsn, self.peer_last_tsn
            );
            result = ReconfigResult::InProgress;
        }

        // RFC 8831 section 6.7 closes a bidirectional WebRTC data channel by
        // resetting the corresponding outgoing stream after its incoming half
        // is reset. `Stream` models that paired channel, so perform the
        // reciprocal reset automatically.
        //
        // The reciprocal also keeps each unregistered id pending in
        // `reconfigs`, which is what defers `StreamEvent::ResetComplete`
        // until the reciprocal is acknowledged (handle_reconfig_param) or
        // abandoned (on_retransmission_failure).
        if !sis_to_reset.is_empty() {
            let rsn = self.generate_next_rsn();
            let tsn = self.my_next_tsn.wrapping_sub(1);
            let reset_all = p.stream_identifiers.is_empty();
            self.reconfig_reset_streams
                .insert(rsn, sis_to_reset.clone());

            let c = ChunkReconfig {
                param_a: Some(Box::new(ParamOutgoingResetRequest {
                    reconfig_request_sequence_number: rsn,
                    reconfig_response_sequence_number: p.reconfig_request_sequence_number,
                    sender_last_tsn: tsn,
                    stream_identifiers: if reset_all { vec![] } else { sis_to_reset },
                })),
                ..Default::default()
            };

            // Store before queueing. It becomes the active retransmission entry only
            // when gather_outbound actually serializes this packet.
            self.reconfigs.insert(rsn, c.clone());

            let p = self.create_packet(vec![Box::new(c)]);
            reply.push(p);
        }

        if performed {
            self.release_deferred_reset_data()?;
        }

        // Respond to every request that arrived, fresh, retransmitted
        // or deferred by arrival of in-flight data.
        //
        // Intermediate re-evaluations that still fail due to in-flight data
        // stay silent rather than repeating "In progress" on every advance.
        if from_wire || performed {
            let packet = self.create_packet(vec![Box::new(ChunkReconfig {
                param_a: Some(Box::new(ParamReconfigResponse {
                    reconfig_response_sequence_number: p.reconfig_request_sequence_number,
                    result,
                })),
                param_b: None,
            })]);

            debug!("[{}] RESET RESPONSE: {}", self.side, packet);

            reply.push(packet);
        }

        Ok(())
    }

    /// create_packet wraps chunks in a packet.
    /// The caller should hold the read lock.
    pub(crate) fn create_packet(&self, chunks: Vec<Box<dyn Chunk + Send + Sync>>) -> Packet {
        Packet {
            common_header: CommonHeader {
                verification_tag: self.peer_verification_tag,
                source_port: self.source_port,
                destination_port: self.destination_port,
            },
            chunks,
        }
    }

    /// create_stream creates a stream. The caller should hold the lock
    /// and check no stream exists for this id.
    fn create_stream(
        &mut self,
        stream_identifier: StreamId,
        accept: bool,
        default_payload_type: PayloadProtocolIdentifier,
    ) -> Option<Stream<'_>> {
        let generation = self.next_stream_generation;
        self.next_stream_generation = generation.checked_add(1)?;
        let mut s = StreamState::new(
            self.side,
            stream_identifier,
            self.max_payload_size,
            self.max_receive_message_size,
            default_payload_type,
        );

        s.stats.generation = generation;
        s.first_message_id = self.outbound_queue.next_message_id();

        if accept {
            self.stream_queue.push_back(stream_identifier);
            self.events.push_back(Event::Stream(StreamEvent::Opened {
                id: stream_identifier,
            }));
        }

        self.streams.insert(stream_identifier, s);

        Some(Stream {
            stream_identifier,
            association: self,
        })
    }

    /// get_or_create_stream gets or creates a stream. The caller should hold the lock.
    fn get_or_create_stream(&mut self, stream_identifier: StreamId) -> Option<Stream<'_>> {
        if self.streams.contains_key(&stream_identifier) {
            Some(Stream {
                stream_identifier,
                association: self,
            })
        } else {
            self.create_stream(
                stream_identifier,
                true,
                PayloadProtocolIdentifier::default(),
            )
        }
    }

    pub(crate) fn get_my_receiver_window_credit(&self) -> u32 {
        let mut bytes_queued = 0;
        for s in self.streams.values() {
            bytes_queued += s.get_num_bytes_in_reassembly_queue() as u32;
        }
        for chunk in self.deferred_reset_data.values() {
            bytes_queued += chunk.user_data.len() as u32;
        }

        self.max_receive_buffer_size.saturating_sub(bytes_queued)
    }

    /// gather_outbound gathers outgoing packets. The returned bool value set to
    /// false means the association should be closed down after the final send.
    fn gather_outbound(&mut self, now: Instant) -> (Vec<Bytes>, bool) {
        let mut raw_packets = self.gather_outbound_control_packets(vec![], now);

        let state = self.state();
        match state {
            AssociationState::Established => {
                raw_packets = self.gather_data_packets_to_retransmit(raw_packets, now);
                // Decide fast-recovery abandonment before selecting any pending tail.
                raw_packets = self.gather_outbound_fast_retransmission_packets(raw_packets, now);
                raw_packets = self.gather_outbound_data_and_reconfig_packets(raw_packets, now);
                // A queued reciprocal reset may have been held back until the
                // DATA it covers received TSNs above. Give it another chance in
                // this same poll so DATA and RE-CONFIG can leave together.
                raw_packets = self.gather_outbound_control_packets(raw_packets, now);
                raw_packets = self.gather_outbound_sack_packets(raw_packets);
                raw_packets = self.gather_outbound_forward_tsn_packets(raw_packets);
                (raw_packets, true)
            }
            AssociationState::ShutdownPending
            | AssociationState::ShutdownSent
            | AssociationState::ShutdownReceived => {
                raw_packets = self.gather_data_packets_to_retransmit(raw_packets, now);
                raw_packets = self.gather_outbound_fast_retransmission_packets(raw_packets, now);
                raw_packets = self.gather_outbound_sack_packets(raw_packets);
                raw_packets = self.gather_outbound_forward_tsn_packets(raw_packets);
                self.gather_outbound_shutdown_packets(raw_packets, now)
            }
            AssociationState::ShutdownAckSent => {
                self.gather_outbound_shutdown_packets(raw_packets, now)
            }
            _ => (raw_packets, true),
        }
    }

    fn gather_outbound_control_packets(
        &mut self,
        mut raw_packets: Vec<Bytes>,
        now: Instant,
    ) -> Vec<Bytes> {
        if !self.control_queue.is_empty() {
            let mut buffered = VecDeque::new();
            let mut request_blocked = false;
            let queued = core::mem::take(&mut self.control_queue);
            for p in queued {
                let outgoing_rsn = Self::packet_reconfig_request_rsn(&p);

                if outgoing_rsn.is_some() && request_blocked {
                    buffered.push_back(p);
                    continue;
                }

                if outgoing_rsn.is_some_and(|rsn| {
                    self.active_reconfig.is_some() && self.active_reconfig != Some(rsn)
                }) {
                    request_blocked = true;
                    buffered.push_back(p);
                    continue;
                }

                // The reset boundary cannot be fixed until all already-queued
                // DATA for the affected streams has received a TSN.
                if outgoing_rsn.is_some_and(|rsn| self.reconfig_has_pending_data(rsn)) {
                    request_blocked = true;
                    buffered.push_back(p);
                    continue;
                }

                let p = if let Some(rsn) = outgoing_rsn {
                    self.refresh_unsent_reconfig(rsn).unwrap_or(p)
                } else {
                    p
                };

                match p.marshal() {
                    Ok(raw) => {
                        raw_packets.push(raw);
                        if let Some(rsn) = outgoing_rsn {
                            self.active_reconfig = Some(rsn);
                        }
                    }
                    Err(_) => {
                        warn!("[{}] failed to serialize a control packet", self.side);
                        // A queued request owns reset state and must not be lost.
                        if outgoing_rsn.is_some() {
                            request_blocked = true;
                            buffered.push_back(p);
                        }
                    }
                }
            }
            self.control_queue = buffered;
        }

        if self.active_reconfig.is_some() {
            self.timers
                .restart_if_stale(Timer::Reconfig, now, self.rto_mgr.get_rto());
        }

        raw_packets
    }

    fn gather_data_packets_to_retransmit(
        &mut self,
        mut raw_packets: Vec<Bytes>,
        now: Instant,
    ) -> Vec<Bytes> {
        for p in &self.get_data_packets_to_retransmit(now) {
            if let Ok(raw) = p.marshal() {
                raw_packets.push(raw);
            } else {
                warn!(
                    "[{}] failed to serialize a DATA packet to be retransmitted",
                    self.side
                );
            }
        }

        raw_packets
    }

    fn gather_outbound_data_and_reconfig_packets(
        &mut self,
        mut raw_packets: Vec<Bytes>,
        now: Instant,
    ) -> Vec<Bytes> {
        // Pop unsent data chunks from the pending queue to send as much as
        // cwnd and rwnd allow.
        let chunks = self.pop_pending_data_chunks_to_send(now);
        if !chunks.is_empty() {
            // Start timer. (noop if already started)
            trace!("[{}] T3-rtx timer start (pt1)", self.side);
            self.timers
                .restart_if_stale(Timer::T3RTX, now, self.rto_mgr.get_rto());

            for p in &self.bundle_data_chunks_into_packets(chunks) {
                if let Ok(raw) = p.marshal() {
                    raw_packets.push(raw);
                } else {
                    warn!("[{}] failed to serialize a DATA packet", self.side);
                }
            }
        }

        if self.will_retransmit_reconfig {
            self.will_retransmit_reconfig = false;
            if let Some(rsn) = self.active_reconfig {
                debug!("[{}] retransmit RECONFIG rsn={}", self.side, rsn);
                if let Some(c) = self.reconfigs.get(&rsn) {
                    let p = self.create_packet(vec![Box::new(c.clone())]);
                    if let Ok(raw) = p.marshal() {
                        raw_packets.push(raw);
                    } else {
                        warn!(
                            "[{}] failed to serialize a RECONFIG packet to be retransmitted",
                            self.side,
                        );
                    }
                }
            }
        }

        // RFC 6525 section 5.1.1 permits only one request in flight. Keep
        // application resets separate from DATA until that request completes.
        if self.active_reconfig.is_none()
            && self.reconfigs.is_empty()
            && !self.pending_reset_streams.is_empty()
        {
            // DATA on an unrelated stream must not starve a reset, nor should
            // one busy stream hold back ready reset requests for other streams.
            let outbound_queue = &self.outbound_queue;
            let mut stream_ids = vec![];
            self.pending_reset_streams.retain(|id| {
                if outbound_queue.pending_contains_stream(*id) {
                    true
                } else {
                    stream_ids.push(*id);
                    false
                }
            });

            if stream_ids.is_empty() {
                return raw_packets;
            }

            let rsn = self.generate_next_rsn();
            let tsn = self.my_next_tsn.wrapping_sub(1);
            debug!(
                "[{}] sending RECONFIG: rsn={} tsn={} streams={:?}",
                self.side, rsn, tsn, stream_ids
            );

            self.reconfig_reset_streams.insert(rsn, stream_ids.clone());
            let c = ChunkReconfig {
                param_a: Some(Box::new(ParamOutgoingResetRequest {
                    reconfig_request_sequence_number: rsn,
                    reconfig_response_sequence_number: self.peer_last_reconfig_rsn,
                    sender_last_tsn: tsn,
                    stream_identifiers: stream_ids,
                })),
                ..Default::default()
            };
            self.reconfigs.insert(rsn, c.clone());

            let p = self.create_packet(vec![Box::new(c)]);
            if let Ok(raw) = p.marshal() {
                self.active_reconfig = Some(rsn);
                raw_packets.push(raw);
            } else {
                warn!(
                    "[{}] failed to serialize a RECONFIG packet to be transmitted",
                    self.side
                );
                // Preserve the request for a later serialization attempt.
                self.control_queue.push_back(p);
            }
        }

        // The timer belongs to exactly the request selected above or while
        // draining the control queue.
        if self.active_reconfig.is_some() {
            self.timers
                .restart_if_stale(Timer::Reconfig, now, self.rto_mgr.get_rto());
        }

        raw_packets
    }

    fn gather_outbound_fast_retransmission_packets(
        &mut self,
        mut raw_packets: Vec<Bytes>,
        now: Instant,
    ) -> Vec<Bytes> {
        self.expire_inflight_messages(now);
        if self.will_retransmit_fast {
            self.will_retransmit_fast = false;

            let mut to_fast_retrans: Vec<Box<dyn Chunk + Send + Sync>> = vec![];
            let mut fast_retrans_size = COMMON_HEADER_SIZE;

            let mut i = 0;
            loop {
                let tsn = self
                    .cumulative_tsn_ack_point
                    .wrapping_add(i)
                    .wrapping_add(1);
                if let Some(c) = self.outbound_queue.get(tsn) {
                    if c.acked || c.abandoned || c.nsent > 1 || c.miss_indicator < 3 {
                        i += 1;
                        continue;
                    }

                    // RFC 4960 Sec 7.2.4 Fast Retransmit on Gap Reports
                    //  3)  Determine how many of the earliest (i.e., lowest TSN) DATA chunks
                    //      marked for retransmission will fit into a single packet, subject
                    //      to constraint of the path MTU of the destination transport
                    //      address to which the packet is being sent.  Call this value K.
                    //      Retransmit those K DATA chunks in a single packet.  When a Fast
                    //      Retransmit is being performed, the sender SHOULD ignore the value
                    //      of cwnd and SHOULD NOT delay retransmission for this single
                    //		packet.

                    if self.abandon_if_needed(tsn, now) {
                        i += 1;
                        continue;
                    }

                    let c = self.outbound_queue.get_mut(tsn).unwrap();
                    let data_chunk_size = DATA_CHUNK_HEADER_SIZE + c.user_data.len() as u32;
                    if self.mtu < fast_retrans_size + data_chunk_size {
                        break;
                    }

                    fast_retrans_size += data_chunk_size;
                    self.stats.inc_fast_retrans();
                    c.nsent += 1;
                } else {
                    break; // end of pending data
                }

                if let Some(c) = self.outbound_queue.get_mut(tsn) {
                    to_fast_retrans.push(Box::new(c.clone()));
                    trace!(
                        "[{}] fast-retransmit: tsn={} sent={} htna={}",
                        self.side, c.tsn, c.nsent, self.fast_recover_exit_point
                    );
                }
                i += 1;
            }

            to_fast_retrans.retain(|chunk| {
                let data = chunk.as_any().downcast_ref::<ChunkPayloadData>().unwrap();
                !self.outbound_queue.is_abandoned(data.tsn)
            });
            if !to_fast_retrans.is_empty() {
                if let Ok(raw) = self.create_packet(to_fast_retrans).marshal() {
                    raw_packets.push(raw);
                } else {
                    warn!(
                        "[{}] failed to serialize a DATA packet to be fast-retransmitted",
                        self.side
                    );
                }
            }
        }

        raw_packets
    }

    fn gather_outbound_sack_packets(&mut self, mut raw_packets: Vec<Bytes>) -> Vec<Bytes> {
        if self.ack_state == AckState::Immediate {
            self.ack_state = AckState::Idle;
            let sack = self.create_selective_ack_chunk();
            trace!("[{}] sending SACK: {}", self.side, sack);
            if let Ok(raw) = self.create_packet(vec![Box::new(sack)]).marshal() {
                raw_packets.push(raw);
            } else {
                warn!("[{}] failed to serialize a SACK packet", self.side);
            }
        }

        raw_packets
    }

    /// RFC 3758 section 3.5 C1-C2: advance over abandoned messages' sent TSNs.
    fn advance_peer_tsn_ack_point(&mut self) {
        if sna32lt(
            self.advanced_peer_tsn_ack_point,
            self.cumulative_tsn_ack_point,
        ) {
            self.advanced_peer_tsn_ack_point = self.cumulative_tsn_ack_point;
        }
        let mut next = self.advanced_peer_tsn_ack_point.wrapping_add(1);
        while self.outbound_queue.is_abandoned(next) {
            self.advanced_peer_tsn_ack_point = next;
            next = next.wrapping_add(1);
        }
    }

    fn gather_outbound_forward_tsn_packets(&mut self, mut raw_packets: Vec<Bytes>) -> Vec<Bytes> {
        // Retransmission decisions above may have just abandoned DATA. Advance
        // now, including after a timeout with no subsequent incoming SACK.
        if self.use_forward_tsn {
            let previous = self.advanced_peer_tsn_ack_point;
            self.advance_peer_tsn_ack_point();
            if sna32gt(self.advanced_peer_tsn_ack_point, previous)
                && sna32gt(
                    self.advanced_peer_tsn_ack_point,
                    self.cumulative_tsn_ack_point,
                )
            {
                self.will_send_forward_tsn = true;
            }
        }
        /*log::debug!(
            "[{}] gatherOutboundForwardTSNPackets {}",
            self.name,
            self.will_send_forward_tsn
        );*/
        if self.will_send_forward_tsn {
            self.will_send_forward_tsn = false;
            if sna32gt(
                self.advanced_peer_tsn_ack_point,
                self.cumulative_tsn_ack_point,
            ) {
                let fwd_tsn = self.create_forward_tsn();
                if let Ok(raw) = self.create_packet(vec![Box::new(fwd_tsn)]).marshal() {
                    raw_packets.push(raw);
                } else {
                    warn!("[{}] failed to serialize a Forward TSN packet", self.side);
                }
            }
        }

        raw_packets
    }

    fn gather_outbound_shutdown_packets(
        &mut self,
        mut raw_packets: Vec<Bytes>,
        now: Instant,
    ) -> (Vec<Bytes>, bool) {
        let mut ok = true;

        if self.will_send_shutdown {
            self.will_send_shutdown = false;

            let shutdown = ChunkShutdown {
                cumulative_tsn_ack: self.cumulative_tsn_ack_point,
            };

            if let Ok(raw) = self.create_packet(vec![Box::new(shutdown)]).marshal() {
                self.timers
                    .start(Timer::T2Shutdown, now, self.rto_mgr.get_rto());
                raw_packets.push(raw);
            } else {
                warn!("[{}] failed to serialize a Shutdown packet", self.side);
            }
        } else if self.will_send_shutdown_ack {
            self.will_send_shutdown_ack = false;

            let shutdown_ack = ChunkShutdownAck {};

            if let Ok(raw) = self.create_packet(vec![Box::new(shutdown_ack)]).marshal() {
                self.timers
                    .start(Timer::T2Shutdown, now, self.rto_mgr.get_rto());
                raw_packets.push(raw);
            } else {
                warn!("[{}] failed to serialize a ShutdownAck packet", self.side);
            }
        } else if self.will_send_shutdown_complete {
            self.will_send_shutdown_complete = false;

            let shutdown_complete = ChunkShutdownComplete {};

            if let Ok(raw) = self
                .create_packet(vec![Box::new(shutdown_complete)])
                .marshal()
            {
                raw_packets.push(raw);
                ok = false;
            } else {
                warn!(
                    "[{}] failed to serialize a ShutdownComplete packet",
                    self.side
                );
            }
        }

        (raw_packets, ok)
    }

    /// get_data_packets_to_retransmit is called when T3-rtx is timed
    /// out and retransmit outstanding data chunks that are not acked
    /// or abandoned yet.
    fn get_data_packets_to_retransmit(&mut self, now: Instant) -> Vec<Packet> {
        self.expire_inflight_messages(now);
        let awnd = core::cmp::min(self.cwnd, self.rwnd);
        let mut chunks: Vec<ChunkPayloadData> = vec![];
        let mut bytes_to_send = 0;
        let mut done = false;
        let mut i = 0;
        while !done {
            let tsn = self
                .cumulative_tsn_ack_point
                .wrapping_add(i)
                .wrapping_add(1);
            if let Some(c) = self.outbound_queue.get(tsn) {
                if !c.retransmit {
                    i += 1;
                    continue;
                }

                if self.abandon_if_needed(tsn, now) {
                    chunks.retain(|c| !self.outbound_queue.is_abandoned(c.tsn));
                    bytes_to_send = chunks.iter().map(|c| c.user_data.len()).sum();
                    i += 1;
                    continue;
                }

                let c = self.outbound_queue.get_mut(tsn).unwrap();
                if i == 0 && self.rwnd < c.user_data.len() as u32 {
                    // Send it as a zero window probe
                    done = true;
                } else if bytes_to_send + c.user_data.len() > awnd as usize {
                    break;
                }

                bytes_to_send += c.user_data.len();
            } else {
                break; // end of pending data
            }

            if let Some(c) = self.outbound_queue.get_mut(tsn) {
                trace!(
                    "[{}] retransmitting tsn={} ssn={} sent={}",
                    self.side, c.tsn, c.stream_sequence_number, c.nsent
                );

                chunks.push(c.clone());
            }
            i += 1;
        }

        // A later fragment may exhaust the message's policy after an earlier
        // fragment was selected in this pass. Neither should be retransmitted.
        for chunk in &mut chunks {
            let c = self.outbound_queue.get_mut(chunk.tsn).unwrap();
            c.retransmit = false;
            c.nsent += 1;
            chunk.nsent = c.nsent;
        }
        self.bundle_data_chunks_into_packets(chunks)
    }

    /// pop_pending_data_chunks_to_send pops chunks from the pending queues as many as
    /// the cwnd and rwnd allows to send.
    fn pop_pending_data_chunks_to_send(&mut self, now: Instant) -> Vec<ChunkPayloadData> {
        self.expire_inflight_messages(now);
        let mut chunks = vec![];
        if self.outbound_queue.pending_len() != 0 {
            // RFC 4960 sec 6.1.  Transmission of DATA Chunks
            //   A) At any given time, the data sender MUST NOT transmit new data to
            //      any destination transport address if its peer's rwnd indicates
            //      that the peer has no buffer space (i.e., rwnd is 0; see Section
            //      6.2.1).  However, regardless of the value of rwnd (including if it
            //      is 0), the data sender can always have one DATA chunk in flight to
            //      the receiver if allowed by cwnd (see rule B, below).

            loop {
                if self.use_forward_tsn {
                    if let Some((id, unordered, bytes)) = self.outbound_queue.expire_pending(now) {
                        if let Some(stream) = self.streams.get_mut(&id) {
                            if !unordered {
                                stream.sequence_number = stream.sequence_number.wrapping_sub(1);
                            }
                            if stream.on_buffer_released(bytes as i64) {
                                self.events.push_back(Event::Stream(
                                    StreamEvent::BufferedAmountLow { id },
                                ));
                            }
                        }
                        continue;
                    }
                }
                let Some(c) = self.outbound_queue.peek_pending() else {
                    break;
                };
                let data_len = c.user_data.len();

                if self.outbound_queue.inflight_bytes() + data_len > self.cwnd as usize {
                    break; // would exceeds cwnd
                }

                if data_len > self.rwnd as usize {
                    break; // no more rwnd
                }

                self.rwnd -= data_len as u32;

                if let Some(chunk) = self.send_next_data_chunk(now) {
                    chunks.push(chunk);
                }
            }

            // the data sender can always have one DATA chunk in flight to the receiver
            if chunks.is_empty() && self.outbound_queue.inflight_len() == 0 {
                // Send zero window probe
                if let Some(chunk) = self.send_next_data_chunk(now) {
                    chunks.push(chunk);
                }
            }
        }

        chunks
    }

    /// bundle_data_chunks_into_packets packs DATA chunks into packets. It tries to bundle
    /// DATA chunks into a packet so long as the resulting packet size does not exceed
    /// the path MTU.
    fn bundle_data_chunks_into_packets(&self, chunks: Vec<ChunkPayloadData>) -> Vec<Packet> {
        let mut packets = vec![];
        let mut chunks_to_send = vec![];
        let mut bytes_in_packet = COMMON_HEADER_SIZE;

        for c in chunks {
            // RFC 4960 sec 6.1.  Transmission of DATA Chunks
            //   Multiple DATA chunks committed for transmission MAY be bundled in a
            //   single packet.  Furthermore, DATA chunks being retransmitted MAY be
            //   bundled with new DATA chunks, as long as the resulting packet size
            //   does not exceed the path MTU.
            if bytes_in_packet + c.user_data.len() as u32 > self.mtu {
                packets.push(self.create_packet(chunks_to_send));
                chunks_to_send = vec![];
                bytes_in_packet = COMMON_HEADER_SIZE;
            }

            bytes_in_packet += DATA_CHUNK_HEADER_SIZE + c.user_data.len() as u32;
            chunks_to_send.push(Box::new(c));
        }

        if !chunks_to_send.is_empty() {
            packets.push(self.create_packet(chunks_to_send));
        }

        packets
    }

    /// generate_next_rsn returns the my_next_rsn and increases it. The caller should hold the lock.
    fn generate_next_rsn(&mut self) -> u32 {
        let rsn = self.my_next_rsn;
        self.my_next_rsn = self.my_next_rsn.wrapping_add(1);
        rsn
    }

    /// Timed messages expire at their deadline, before any DATA is selected.
    /// In particular, a blocked early chunk must not hide later expired DATA.
    fn expire_inflight_messages(&mut self, now: Instant) {
        if self.use_forward_tsn {
            self.outbound_queue.start_timed_messages(now);
            let mut abandoned = false;
            for tsn in self.outbound_queue.expired_inflight(now) {
                abandoned |= self.abandon_if_needed(tsn, now);
            }
            while let Some((id, message_id, bytes)) = self
                .outbound_queue
                .expire_pending_tail(now, &mut self.my_next_tsn)
            {
                self.release_abandoned_buffer(id, message_id, bytes);
                abandoned = true;
            }
            if abandoned {
                self.timers
                    .restart_if_stale(Timer::T3RTX, now, self.rto_mgr.get_rto());
            }
        }
    }

    /// Evaluate retry candidates or messages whose lifetime expired.
    /// Abandoning a message also drops its unsent tail.
    fn abandon_if_needed(&mut self, tsn: u32, now: Instant) -> bool {
        let Some(c) = self.outbound_queue.get(tsn) else {
            return false;
        };
        if c.abandoned {
            return true;
        }
        if !self.use_forward_tsn {
            return false;
        }
        // Data-channel establishment messages always use reliable delivery.
        if c.payload_type == PayloadProtocolIdentifier::Dcep {
            return false;
        }
        let Some((reliability_type, reliability_value)) = c.reliability.or_else(|| {
            self.streams
                .get(&c.stream_identifier)
                .map(|stream| (stream.reliability_type, stream.reliability_value))
        }) else {
            return false;
        };
        let expired = match reliability_type {
            // nsent includes the original send; maxRTX counts only retries.
            ReliabilityType::Rexmit => c.nsent > reliability_value,
            ReliabilityType::Timed => c.deadline.is_some_and(|deadline| now >= deadline),
            ReliabilityType::Reliable => false,
        };
        if !expired {
            return false;
        }
        let message_id = c.message_id;
        trace!("[{}] abandoning message containing tsn={}", self.side, tsn);
        if let Some((id, bytes)) = self.outbound_queue.abandon(tsn, &mut self.my_next_tsn) {
            self.release_abandoned_buffer(id, message_id, bytes);
        }
        true
    }

    fn release_abandoned_buffer(&mut self, id: u16, message_id: u64, bytes: usize) {
        if let Some(stream) = self.streams.get_mut(&id) {
            if message_id >= stream.first_message_id && stream.on_buffer_released(bytes as i64) {
                self.events
                    .push_back(Event::Stream(StreamEvent::BufferedAmountLow { id }));
            }
        }
    }

    fn create_selective_ack_chunk(&mut self) -> ChunkSelectiveAck {
        ChunkSelectiveAck {
            cumulative_tsn_ack: self.peer_last_tsn,
            advertised_receiver_window_credit: self.get_my_receiver_window_credit(),
            gap_ack_blocks: self.payload_queue.get_gap_ack_blocks(self.peer_last_tsn),
            duplicate_tsn: self.payload_queue.pop_duplicates(),
        }
    }

    /// create_forward_tsn generates ForwardTSN chunk.
    /// This method is called when use_forward_tsn is set to true.
    fn create_forward_tsn(&self) -> ChunkForwardTsn {
        // RFC 3758 Sec 3.5 C4
        let mut stream_map: HashMap<u16, u16> = HashMap::new(); // to report only once per SI
        let mut i = self.cumulative_tsn_ack_point.wrapping_add(1);
        while sna32lte(i, self.advanced_peer_tsn_ack_point) {
            if let Some(c) = self.outbound_queue.get(i) {
                // RFC 3758 C4: unordered DATA advances only the TSN point.
                if c.unordered {
                    i = i.wrapping_add(1);
                    continue;
                }
                if let Some(ssn) = stream_map.get(&c.stream_identifier) {
                    if sna16lt(*ssn, c.stream_sequence_number) {
                        // to report only once with greatest SSN
                        stream_map.insert(c.stream_identifier, c.stream_sequence_number);
                    }
                } else {
                    stream_map.insert(c.stream_identifier, c.stream_sequence_number);
                }
            } else {
                break;
            }

            i = i.wrapping_add(1);
        }

        let mut fwd_tsn = ChunkForwardTsn {
            new_cumulative_tsn: self.advanced_peer_tsn_ack_point,
            streams: vec![],
        };

        let mut stream_str = String::new();
        for (si, ssn) in &stream_map {
            stream_str += format!("(si={} ssn={})", si, ssn).as_str();
            fwd_tsn.streams.push(ChunkForwardTsnStream {
                identifier: *si,
                sequence: *ssn,
            });
        }
        trace!(
            "[{}] building fwd_tsn: newCumulativeTSN={} cumTSN={} - {}",
            self.side, fwd_tsn.new_cumulative_tsn, self.cumulative_tsn_ack_point, stream_str
        );

        fwd_tsn
    }

    /// Assign a TSN to the next pending fragment and retain it for retransmission.
    fn send_next_data_chunk(&mut self, now: Instant) -> Option<ChunkPayloadData> {
        let chunk = self.outbound_queue.send_next(&mut self.my_next_tsn, now)?;
        trace!(
            "[{}] sending ppi={} tsn={} ssn={} len={}",
            self.side,
            chunk.payload_type,
            chunk.tsn,
            chunk.stream_sequence_number,
            chunk.user_data.len()
        );
        Some(chunk)
    }

    pub(crate) fn send_reset_request(&mut self, stream_identifier: StreamId) -> Result<()> {
        let state = self.state();
        if state != AssociationState::Established {
            return Err(Error::ErrResetPacketInStateNotExist);
        }

        self.pending_reset_streams.push_back(stream_identifier);
        self.awake_write_loop();

        Ok(())
    }

    /// send_payload_data sends the data chunks.
    pub(crate) fn send_payload_data(&mut self, message: Vec<ChunkPayloadData>) -> Result<()> {
        let state = self.state();
        if state != AssociationState::Established {
            return Err(Error::ErrPayloadDataStateNotExist);
        }

        self.outbound_queue.push(message);

        self.awake_write_loop();
        Ok(())
    }

    /// buffered_amount returns total amount (in bytes) of currently buffered user data.
    /// This is used only by testing.
    pub(crate) fn buffered_amount(&self) -> usize {
        self.outbound_queue.pending_bytes() + self.outbound_queue.inflight_bytes()
    }

    fn awake_write_loop(&self) {
        // No Op on Purpose
    }

    fn close_all_timers(&mut self) {
        // Close all retransmission & ack timers
        for timer in Timer::VALUES {
            self.timers.stop(timer);
        }
    }

    fn on_ack_timeout(&mut self) {
        trace!(
            "[{}] ack timed out (ack_state: {})",
            self.side, self.ack_state
        );
        self.stats.inc_ack_timeouts();
        self.ack_state = AckState::Immediate;
        self.awake_write_loop();
    }

    fn on_retransmission_timeout(&mut self, timer_id: Timer, n_rtos: usize) {
        match timer_id {
            Timer::T1Init => {
                if let Err(err) = self.send_init() {
                    debug!(
                        "[{}] failed to retransmit init (n_rtos={}): {:?}",
                        self.side, n_rtos, err
                    );
                }
            }

            Timer::T1Cookie => {
                if let Err(err) = self.send_cookie_echo() {
                    debug!(
                        "[{}] failed to retransmit cookie-echo (n_rtos={}): {:?}",
                        self.side, n_rtos, err
                    );
                }
            }

            Timer::T2Shutdown => {
                debug!(
                    "[{}] retransmission of shutdown timeout (n_rtos={})",
                    self.side, n_rtos
                );
                let state = self.state();
                match state {
                    AssociationState::ShutdownSent => {
                        self.will_send_shutdown = true;
                        self.awake_write_loop();
                    }
                    AssociationState::ShutdownAckSent => {
                        self.will_send_shutdown_ack = true;
                        self.awake_write_loop();
                    }
                    _ => {}
                }
            }

            Timer::T3RTX => {
                self.stats.inc_t3timeouts();

                // RFC 4960 sec 6.3.3
                //  E1)  For the destination address for which the timer expires, adjust
                //       its ssthresh with rules defined in Section 7.2.3 and set the
                //       cwnd <- MTU.
                // RFC 4960 sec 7.2.3
                //   When the T3-rtx timer expires on an address, SCTP should perform slow
                //   start by:
                //      ssthresh = max(cwnd/2, 4*MTU)
                //      cwnd = 1*MTU

                self.ssthresh = core::cmp::max(self.cwnd / 2, 4 * self.mtu);
                self.cwnd = self.mtu;
                trace!(
                    "[{}] updated cwnd={} ssthresh={} inflight={} (RTO)",
                    self.side,
                    self.cwnd,
                    self.ssthresh,
                    self.outbound_queue.inflight_bytes()
                );

                // RFC 3758 sec 3.5
                //  A5) Any time the T3-rtx timer expires, on any destination, the sender
                //  SHOULD try to advance the "Advanced.Peer.Ack.Point" by following
                //  the procedures outlined in C2 - C5.
                if self.use_forward_tsn {
                    self.advance_peer_tsn_ack_point();

                    // RFC 3758 Sec 3.5 C3
                    if sna32gt(
                        self.advanced_peer_tsn_ack_point,
                        self.cumulative_tsn_ack_point,
                    ) {
                        self.will_send_forward_tsn = true;
                        debug!(
                            "[{}] on_retransmission_timeout {}: sna32GT({}, {})",
                            self.side,
                            self.will_send_forward_tsn,
                            self.advanced_peer_tsn_ack_point,
                            self.cumulative_tsn_ack_point
                        );
                    }
                }

                debug!(
                    "[{}] T3-rtx timed out: n_rtos={} cwnd={} ssthresh={}",
                    self.side, n_rtos, self.cwnd, self.ssthresh
                );

                self.outbound_queue.mark_all_to_retransmit();
                self.awake_write_loop();
            }

            Timer::Reconfig => {
                self.will_retransmit_reconfig = true;
                self.awake_write_loop();
            }

            _ => {}
        }
    }

    fn on_retransmission_failure(&mut self, id: Timer) {
        match id {
            Timer::T1Init => {
                error!("[{}] retransmission failure: T1-init", self.side);
                self.error = Some(AssociationError::HandshakeFailed(
                    Error::ErrHandshakeInitAck,
                ));
            }

            Timer::T1Cookie => {
                error!("[{}] retransmission failure: T1-cookie", self.side);
                self.error = Some(AssociationError::HandshakeFailed(
                    Error::ErrHandshakeCookieEcho,
                ));
            }

            Timer::T2Shutdown => {
                error!("[{}] retransmission failure: T2-shutdown", self.side);
            }

            Timer::T3RTX => {
                // T3-rtx timer will not fail by design
                // Justifications:
                //  * ICE would fail if the connectivity is lost
                //  * WebRTC spec is not clear how this incident should be reported to ULP
                error!("[{}] retransmission failure: T3-rtx (DATA)", self.side);
            }

            Timer::Reconfig => {
                if let Some(rsn) = self.active_reconfig {
                    error!(
                        "[{}] retransmission failure: Reconfig rsn={}",
                        self.side, rsn
                    );
                    self.finish_reconfig(rsn, Err(StreamResetError::Failed));
                } else {
                    self.timers.stop(Timer::Reconfig);
                }
            }

            _ => {}
        }
    }

    /// Whether no timers are running
    #[cfg(test)]
    pub(crate) fn is_idle(&self) -> bool {
        Timer::VALUES
            .iter()
            //.filter(|&&t| t != Timer::KeepAlive && t != Timer::PushNewCid)
            .filter_map(|&t| Some((t, self.timers.get(t)?)))
            .min_by_key(|&(_, time)| time)
            //.map_or(true, |(timer, _)| timer == Timer::Idle)
            .is_none()
    }
}
