#![allow(clippy::new_without_default)]

use std::collections::{HashSet, VecDeque};
use std::fmt;
use std::net::SocketAddr;
use std::panic::UnwindSafe;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sctp_proto::{Association, AssociationHandle, DatagramEvent};
use sctp_proto::{Endpoint, EndpointConfig, Stream, StreamEvent, Transmit};
use sctp_proto::{Event, Payload, PayloadProtocolIdentifier, ServerConfig};

use snap::{b64_encode, webrtc_transport_config};

use crate::poll::Wake;

pub use sctp_proto::Error as ProtoError;
use sctp_proto::ReliabilityType;

mod snap;
pub use snap::SctpInitData;

mod dcep;
use dcep::DcepAck;
use dcep::DcepOpen;

mod error;
pub use error::SctpError;

/// Default bytes that can be buffered inside str0m across all streams.
///
/// Configured by `RtcConfig::set_sctp_max_buffered_amount`.
pub(crate) const DEFAULT_MAX_BUFFERED_AMOUNT: usize = 128 * 1024;

/// Default maximum message size we advertise in SDP (what we can receive).
pub const LOCAL_MAX_MESSAGE_SIZE: u32 = 256 * 1024;

/// Default max message size if remote doesn't advertise
pub const DEFAULT_REMOTE_MAX_MESSAGE_SIZE: u32 = 64 * 1024;

pub(crate) struct RtcSctp {
    state: RtcSctpState,
    endpoint: Endpoint,
    fake_addr: SocketAddr,
    handle: AssociationHandle,
    assoc: Option<Association>,
    // Sorted by `id` so lookups can binary search. Keep it that way.
    entries: Vec<StreamEntry>,
    // Stream ids that still owe a `StreamEvent::ResetComplete`,
    // thos ids needs to be held back from reuse.
    reset_pending: HashSet<u16>,
    // Completed resets awaiting delivery to the caller.
    // Used to guarantee emission ordering, ResetComplete must
    // be sent after Close.
    reset_complete: VecDeque<u16>,
    pushed_back_transmit: Option<VecDeque<Vec<u8>>>,
    last_now: Instant,
    client: bool,
    local_max_message_size: u32,
    remote_max_message_size: u32,
    snap_enabled: bool,
    snap_init: Option<SctpInitData>,
    /// Bytes that can be buffered across all streams before `available()` is 0.
    max_buffered_amount: usize,
    #[cfg(test)]
    max_payload_size: usize,
}

/// This is okay because there is no way for a user of Rtc to interact with the Sctp subsystem
/// in a way that would allow them to observe a potentially broken invariant when catching a panic.
impl UnwindSafe for RtcSctp {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RtcSctpState {
    Uninited,
    AwaitRemoteAssociation,
    AwaitAssociationEstablished,
    Established,
}

impl RtcSctpState {
    pub fn propagate_endpoint_to_assoc(&self) -> bool {
        matches!(
            self,
            RtcSctpState::AwaitAssociationEstablished | RtcSctpState::Established
        )
    }
}

#[derive(Debug)]
struct StreamEntry {
    /// Config as provided when opening the channel. This is None if we discover
    /// the channel from the remote peer before getting a DcepOpen or local open_stream.
    config: Option<ChannelConfig>,
    /// Current state
    state: StreamEntryState,
    /// Actual stream id. Negotiated or automatically allocated.
    id: u16,
    /// If we are to close this entry.
    do_close: bool,
    /// Deadline for retrying `open_stream` when it fails.
    open_deadline: Option<Instant>,
    /// If the queued outgoing data drops below this threshold, Rtc is to emit an
    /// event to the user.
    buffered_threshold: BufferedThresholdConfig,
}

#[derive(Debug)]
/// Tracks the `buffered_amount_low_threshold` for a stream.
///
/// Lets us defer applying user changes to the underlying SCTP
/// stream until the next poll cycle, without first querying the current
/// configured value.
enum BufferedThresholdConfig {
    /// No threshold has been set or it was cleared after an error.
    Unconfigured,
    /// A user-requested threshold to apply on the next poll.
    Desired(usize),
    /// The threshold value currently configured in the SCTP stream.
    Configured(usize),
}

impl BufferedThresholdConfig {
    pub fn set(&mut self, v: usize) {
        let is_change = match self {
            BufferedThresholdConfig::Unconfigured => true,
            BufferedThresholdConfig::Desired(w) if v != *w => true,
            BufferedThresholdConfig::Configured(x) if v != *x => true,
            _ => false,
        };
        if is_change {
            *self = BufferedThresholdConfig::Desired(v);
        }
    }
}

pub(crate) enum SctpEvent {
    Transmit {
        packets: VecDeque<Vec<u8>>,
    },
    Open {
        id: u16,
        label: String,
    },
    Close {
        id: u16,
        /// Whether a reset handshake is outstanding for this stream id.
        reset_pending: bool,
    },
    /// The reset handshake for a closed stream completed and sctp-proto emitted
    /// StreamEvent::ResetComplete. The stream id is now safe to be used again.
    ///
    /// Always reported after the `Close` for the same id.
    StreamResetComplete {
        id: u16,
    },
    Data {
        id: u16,
        binary: bool,
        data: Vec<u8>,
    },
    BufferedAmountLow {
        id: u16,
    },
    AssociationLost,
}

/// These are the possible paths:
/// ```text
/// local inited, in-band                                     AwaitOpen -> AwaitDcepAck -> Open
/// local inited, out-of-band                                 AwaitOpen                 -> Open
/// remote inited, in-band     AwaitConfig -> (receive dcep)                            -> Open
/// remote inited, out-of-band AwaitConfig -> (open_stream)                             -> Open
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamEntryState {
    /// A new stream declared locally, not discovered from remote.
    AwaitOpen,
    /// A new stream, discovered from remote. It can either be in-band or out-of band
    /// We will either receive DcepOpen in-band, or a open_stream() call out-of-band.
    AwaitConfig,
    /// If we have sent DcepOpen and are waiting for the ack.
    AwaitDcepAck,
    /// Stream is open, ready to send data.
    Open,
    /// If some error occurs.
    Closed,
}

/// (Low level) configuration for a data channel.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChannelConfig {
    /// The label to use for the user to identify the channel.
    pub label: String,
    /// Whether channel is guaranteed ordered delivery of messages.
    pub ordered: bool,
    /// The reliability setting, which can allow to drop messages.
    pub reliability: Reliability,
    /// Whether channel is negotiated in-band (DCEP) or out-of-band.
    /// None means in-band negotiated. Some(stream_id) means out-of-band.
    pub negotiated: Option<u16>,
    /// Protocol name.
    ///
    /// Defaults to ""
    pub protocol: String,
}

impl Default for ChannelConfig {
    fn default() -> Self {
        Self {
            label: Default::default(),
            ordered: true,
            reliability: Default::default(),
            negotiated: Default::default(),
            protocol: Default::default(),
        }
    }
}

/// Reliability setting of a data channel.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Reliability {
    /// Packets are delivered in order, with retransmits.
    #[default]
    Reliable,
    /// Packets delivered out of order with a max lifetime.
    MaxPacketLifetime {
        /// The lifetime of a packet in milliseconds.
        lifetime: u16,
    },
    /// Packets delivered out of order with a max number of retransmits.
    MaxRetransmits {
        /// Number of retransmits before giving up.
        retransmits: u16,
    },
}

impl StreamEntry {
    fn set_state(&mut self, state: StreamEntryState) -> bool {
        if self.state == state {
            return false;
        }
        debug!("Stream {:?} -> {:?}", self.state, state);
        self.state = state;
        true
    }

    #[must_use]
    fn configure_reliability(&mut self, stream: &mut Stream) -> bool {
        let dcep: DcepOpen = self.config.as_ref().expect("config to be set").into();

        let ret = stream.set_reliability_params(
            dcep.unordered,
            dcep.channel_type,
            dcep.reliability_parameter,
        );

        if let Err(e) = ret {
            warn!(
                "Failed to set reliability params on stream {}: {:?}",
                self.id, e
            );
            self.do_close = true;
            return false;
        }

        true
    }
}

/// SCTP framing per outgoing datagram: 12-byte common header + ~16-byte DATA
/// chunk header + some extra.
const SCTP_OVERHEAD: usize = 40;

/// How long `open_stream` keeps retrying when blocked by a transient error
/// which could happen during reset handshake (RFC 6525).
/// Past this the peer is considered broken and the open gives up.
const STREAM_OPEN_TIMEOUT: Duration = Duration::from_secs(10);

/// Backstop poll interval while an open is retrying. Retries are normally
/// driven by the network input that unblocks them,
/// either the reciprocal reset or RECONFIG-RESPONSE arriving.
/// This interval guarantees progress in case of absence of the latter messages.
const STREAM_OPEN_RETRY_INTERVAL: Duration = Duration::from_millis(500);

/// Empirical: SCTP `max_payload_size` of 1200 has produced 1277-byte DTLS-wrapped
/// datagrams (77 bytes combined SCTP + DTLS overhead).
const _: () = assert!(
    crate::io::MAX_DTLS_OVERHEAD + SCTP_OVERHEAD >= 80,
    "MAX_DTLS_OVERHEAD + SCTP_OVERHEAD must cover observed 77-byte SCTP-over-DTLS overhead"
);

impl RtcSctp {
    pub fn new(mtu: usize, local_max_message_size: u32) -> Self {
        let mut config = EndpointConfig::default();
        let max_payload = mtu
            .saturating_sub(crate::io::MAX_DTLS_OVERHEAD)
            .saturating_sub(SCTP_OVERHEAD);
        config.max_payload_size(max_payload as u32);
        #[cfg(test)]
        let max_payload_size = max_payload;
        let mut server_config = ServerConfig::default();
        server_config.transport = webrtc_transport_config(local_max_message_size);
        let endpoint = Endpoint::new(Arc::new(config), Some(Arc::new(server_config)));
        let fake_addr = "1.1.1.1:5000".parse().unwrap();

        RtcSctp {
            state: RtcSctpState::Uninited,
            endpoint,
            fake_addr,
            handle: AssociationHandle(0), // temporary
            assoc: None,
            entries: Vec::new(),
            reset_pending: HashSet::new(),
            reset_complete: VecDeque::new(),
            pushed_back_transmit: None,
            last_now: Instant::now(), // placeholder until init()
            client: false,
            local_max_message_size,
            remote_max_message_size: DEFAULT_REMOTE_MAX_MESSAGE_SIZE,
            snap_enabled: false,
            snap_init: None,
            max_buffered_amount: DEFAULT_MAX_BUFFERED_AMOUNT,
            #[cfg(test)]
            max_payload_size,
        }
    }

    #[cfg(test)]
    pub(crate) fn max_payload_size(&self) -> usize {
        self.max_payload_size
    }

    pub fn is_inited(&self) -> bool {
        self.state != RtcSctpState::Uninited
    }

    pub fn is_closing(&self) -> bool {
        self.assoc.as_ref().is_some_and(|a| a.is_closing())
    }

    pub fn is_closed(&self) -> bool {
        self.assoc.as_ref().is_none_or(|a| a.is_closed())
    }

    pub fn init(
        &mut self,
        client: bool,
        now: Instant,
        sctp_init_data: Option<SctpInitData>,
        remote_max_message_size: Option<u32>,
    ) -> Result<(), SctpError> {
        if self.state != RtcSctpState::Uninited {
            return Err(SctpError::Proto(ProtoError::Other(
                "SCTP already initialized".into(),
            )));
        }

        self.client = client;
        self.last_now = now;

        if let Some(max_msg_size) = remote_max_message_size {
            self.remote_max_message_size = max_msg_size;
        }

        if let Some(snap_data) = sctp_init_data {
            // SNAP path: both local and remote INIT chunks must be present.
            if snap_data.local_init.is_none() || snap_data.remote_init.is_none() {
                return Err(SctpError::Proto(ProtoError::Other(
                    "SNAP requires both local and remote SCTP INIT chunks".into(),
                )));
            }

            let config = snap_data.into_client_config(self.local_max_message_size);
            debug!(
                "New {} association (out-of-band: true)",
                if client { "local" } else { "server" },
            );
            let (handle, mut assoc) = self
                .endpoint
                .connect(config, self.fake_addr)
                .map_err(|e| SctpError::Proto(ProtoError::Other(e.to_string())))?;
            assoc.set_max_send_message_size(self.remote_max_message_size);
            self.handle = handle;
            self.assoc = Some(assoc);

            // With SNAP, both sides exchanged INIT chunks out-of-band. The
            // sctp-proto association is already in established state (via
            // `with_snap`). We set our state to Established immediately
            // even though DTLS may not be connected yet. This is safe
            // because the `dtls_connected` guard in `do_poll_output`
            // prevents any SCTP packets from flowing until the DTLS
            // handshake completes.
            set_state(&mut self.state, RtcSctpState::Established);
        } else if client {
            // Normal client path: initiate the SCTP association.
            let config = SctpInitData::default().into_client_config(self.local_max_message_size);

            debug!("New local association (out-of-band: false)");
            let (handle, mut assoc) = self
                .endpoint
                .connect(config, self.fake_addr)
                .map_err(|e| SctpError::Proto(ProtoError::Other(e.to_string())))?;
            assoc.set_max_send_message_size(self.remote_max_message_size);
            self.handle = handle;
            self.assoc = Some(assoc);
            set_state(&mut self.state, RtcSctpState::AwaitAssociationEstablished);
        } else {
            // Normal server path: wait for the remote to initiate.
            set_state(&mut self.state, RtcSctpState::AwaitRemoteAssociation);
        }

        Ok(())
    }

    pub fn is_client(&self) -> bool {
        self.client
    }

    pub fn local_max_message_size(&self) -> u32 {
        self.local_max_message_size
    }

    /// Enable SNAP by pre-populating the init data.
    pub fn enable_snap(&mut self) {
        self.snap_enabled = true;
        self.snap_init.get_or_insert_with(SctpInitData::new);
    }

    pub fn set_max_buffered_amount(&mut self, max_buffered_amount: usize) {
        self.max_buffered_amount = max_buffered_amount;
    }

    /// Whether local offers should opt in to SNAP.
    pub fn snap_enabled(&self) -> bool {
        self.snap_enabled
    }

    /// Ensure the local SNAP INIT chunk is generated. Returns `false` if
    /// generation failed (degrades to non-SNAP).
    pub fn ensure_local_snap_init(&mut self) -> bool {
        let init_data = self.snap_init.get_or_insert_with(SctpInitData::new);
        if init_data.local_init_chunk().is_err() {
            self.snap_init = None;
            false
        } else {
            true
        }
    }

    /// Discard pending SNAP negotiation state before SCTP starts.
    ///
    /// This preserves local opt-in for future offers.
    pub fn disable_pending_snap(&mut self) {
        if !self.is_inited() {
            self.snap_init = None;
        }
    }

    /// Get the local INIT chunk as a base64 string for SDP, if applicable.
    ///
    /// Returns `None` when:
    /// - SNAP is not active
    /// - SCTP is established without SNAP (non-SNAP association)
    pub fn local_sctp_init_for_sdp(&self) -> Option<String> {
        let d = self.snap_init.as_ref()?;
        if self.is_inited() && d.remote_init.is_none() {
            // Established non-SNAP association - MUST NOT inject sctp-init.
            return None;
        }
        d.local_init.as_ref().map(|b| b64_encode(b))
    }

    /// Get the cached remote INIT string, if set.
    pub fn snap_remote_init_string(&self) -> Option<String> {
        self.snap_init.as_ref().and_then(|d| d.remote_init_string())
    }

    /// Whether this is an established SNAP association (has remote init).
    pub fn is_snap_established(&self) -> bool {
        self.is_inited()
            && self
                .snap_init
                .as_ref()
                .and_then(|d| d.remote_init.as_ref())
                .is_some()
    }

    /// Set the remote SNAP INIT from a base64 string. Returns `Ok(true)` if
    /// accepted, `Ok(false)` on decode error (degrades to non-SNAP).
    pub fn set_remote_snap_init_string(&mut self, value: &str) -> bool {
        let init_data = self.snap_init.get_or_insert_with(SctpInitData::new);
        match init_data.set_remote_init_string(value) {
            Ok(()) => true,
            Err(_) => {
                self.disable_pending_snap();
                false
            }
        }
    }

    /// Build a cloned `SctpInitData` for passing to `init()`, if both local
    /// and remote INIT chunks are present.
    pub fn build_snap_init_data(&self) -> Option<SctpInitData> {
        let d = self.snap_init.as_ref()?;
        if d.local_init.is_none() || d.remote_init.is_none() {
            return None;
        }
        Some(d.clone())
    }

    /// Opens a new stream.
    pub fn open_stream(&mut self, id: u16, config: ChannelConfig) {
        // The channel might already have arrived via SCTP, and if it is negotiated out-of-band
        // we are waiting for the configuration.
        let entry = stream_entry(
            &mut self.entries,
            id,
            StreamEntryState::AwaitOpen,
            "open_stream",
        );

        let in_band = config.negotiated.is_none();

        // Stream should not already have a config, we are either waiting for DcepOpen, or this is
        // out-of-band configuration, in which case this call is setting the config.
        if entry.config.is_some() {
            warn!("Stream is already configured: {}", id);
            entry.do_close = true;
            return;
        } else {
            entry.config = Some(config);
        }

        // If we are in AwaitConfig, the stream was discovered from the remote peer before
        // we got to do open_stream. This means we _must_ be in the out-of-band track,
        // since we shouldn't call open_stream on remotely started in-band.
        if entry.state == StreamEntryState::AwaitConfig {
            if in_band {
                warn!("open_stream in-band negotiation for remote stream: {}", id);
                entry.do_close = true;
            } else {
                // out-of-band where remote started. We can go to Open, but must configure the local
                // stream for it first.

                // The stream can already be gone even though our entry is
                // still AwaitConfig. Close the channel gracefully instead of panicking.
                let Some(assoc) = self.assoc.as_mut() else {
                    entry.do_close = true;
                    return;
                };
                let Ok(mut stream) = assoc.stream(entry.id) else {
                    entry.do_close = true;
                    return;
                };

                if !entry.configure_reliability(&mut stream) {
                    return;
                }

                entry.set_state(StreamEntryState::Open);
            }
        }
    }

    /// Close stream.
    pub fn close_stream(&mut self, id: u16) {
        if let Some(entry) = entry_by_id_mut(&mut self.entries, id) {
            entry.do_close = true;

            // Explicitly close the sctp stream to allow re-use of the same id.
            let _ = self.sctp_propagate_close(id);
        }
    }

    pub fn close(&mut self) -> Result<(), SctpError> {
        let Some(assoc) = &mut self.assoc else {
            return Ok(());
        };

        if assoc.is_closing() || assoc.is_closed() {
            return Ok(());
        }

        Ok(assoc.shutdown()?)
    }

    pub fn is_open(&self, id: u16) -> bool {
        if self.state != RtcSctpState::Established {
            return false;
        }

        let Some(rec) = entry_by_id(&self.entries, id) else {
            return false;
        };

        rec.state == StreamEntryState::Open
    }

    // TODO: fix sctp-proto so we don't need &mut here.
    pub fn available(&mut self) -> usize {
        let Some(assoc) = &mut self.assoc else {
            return 0;
        };

        // The amount currently buffered.
        let total: usize = self
            .entries
            .iter()
            .filter_map(|e| {
                assoc
                    .stream(e.id)
                    .ok()
                    .and_then(|s| s.buffered_amount().ok())
            })
            .sum();

        self.max_buffered_amount.saturating_sub(total)
    }

    pub fn write(&mut self, id: u16, binary: bool, buf: &[u8]) -> Result<usize, SctpError> {
        if self.state != RtcSctpState::Established || self.is_closing() || self.is_closed() {
            return Err(SctpError::WriteBeforeEstablished);
        }

        let assoc = self
            .assoc
            .as_mut()
            .ok_or(SctpError::WriteBeforeEstablished)?;

        let rec = entry_by_id(&self.entries, id).expect("stream entry for write");

        if rec.state != StreamEntryState::Open {
            return Err(SctpError::WriteBeforeEstablished);
        }

        let mut stream = assoc.stream(id)?;

        let ppi = if binary {
            if buf.is_empty() {
                PayloadProtocolIdentifier::BinaryEmpty
            } else {
                PayloadProtocolIdentifier::Binary
            }
        } else if buf.is_empty() {
            PayloadProtocolIdentifier::StringEmpty
        } else {
            PayloadProtocolIdentifier::String
        };

        Ok(stream.write_with_ppi(buf, ppi)?)
    }

    pub fn buffered_amount(&mut self, id: u16) -> usize {
        let Some(assoc) = self.assoc.as_mut() else {
            return 0;
        };

        let Ok(stream) = assoc.stream(id) else {
            return 0;
        };

        stream.buffered_amount().unwrap_or(0)
    }

    pub fn set_buffered_amount_low_threshold(&mut self, id: u16, threshold: usize) {
        let entry =
            entry_by_id_mut(&mut self.entries, id).expect("stream entry for valid channel id");

        // This update will be propagated on next poll.
        entry.buffered_threshold.set(threshold);
    }

    pub fn handle_input(&mut self, now: Instant, data: &[u8]) {
        trace!("Handle input: {}", data.len());

        // TODO, remove Bytes in sctp and just use &[u8].
        let data = data.to_vec().into();
        let r = self.endpoint.handle(now, self.fake_addr, None, None, data);

        let Some((handle, event)) = r else {
            return;
        };

        match event {
            DatagramEvent::NewAssociation(a) => {
                // In slow or unreliable networks from browsers (use 3g or slow 4g) settings.
                // The browser resends a new associations and str0m would override the previously
                // acked association. Webrtc should use only 1 association.
                if self.assoc.is_some() {
                    return;
                }
                debug!("New remote association");
                // Remote side initiated the association
                self.assoc = Some(a);
                self.handle = handle;
                set_state(&mut self.state, RtcSctpState::AwaitAssociationEstablished);
            }
            DatagramEvent::AssociationEvent(event) => {
                self.assoc
                    .as_mut()
                    .expect("association for event")
                    .handle_event(event);
            }
        }
    }

    pub fn handle_timeout(&mut self, now: Instant, wake: &mut Wake) {
        if self.state == RtcSctpState::Uninited {
            // Need to call `init()` before any timeouts are accepted.
            wake.no_events();
            wake.no_timeout();
            return;
        }

        self.last_now = now;

        // Remove closed entries.
        self.entries.retain(|e| e.state != StreamEntryState::Closed);

        let Some(assoc) = &mut self.assoc else {
            // No association can produce output. Keep the timer wake because
            // advancing last_now can move the stream-open retry deadline.
            wake.no_events();
            return;
        };

        assoc.handle_timeout(now);

        // propagate events between endpoint and association.
        while let Some(e) = assoc.poll_endpoint_event() {
            if let Some(ae) = self.endpoint.handle_event(self.handle, e) {
                assoc.handle_event(ae);
            }
        }
    }

    pub fn poll(&mut self) -> Option<SctpEvent> {
        let r = self.do_poll();

        if let Some(r) = &r {
            trace!("Poll {:?}", r);
        }

        r
    }

    pub fn do_poll(&mut self) -> Option<SctpEvent> {
        // Handshakes, DCEP writes, and deferred stream closes can leave more work.
        // Restart until that work is drained before reporting None: the RTC caches
        // an idle result. Each restart advances a handshake or opens/closes a stream;
        // use a loop so draining many channels does not grow the stack.
        'restart: loop {
            if self.state == RtcSctpState::Uninited {
                // Need to call `init()` before any polling starts.
                return None;
            }

            // Remove closed entries. handle_timeout() also does this, but the
            // remote can reuse a stream id (its reset handshake completed) before
            // our next timeout.
            self.entries.retain(|e| e.state != StreamEntryState::Closed);

            if let Some(t) = self.pushed_back_transmit.take() {
                return Some(SctpEvent::Transmit { packets: t });
            }

            while let Some(t) = self.poll_transmit() {
                let Some(buf) = transmit_to_vec(t) else {
                    continue;
                };

                return Some(SctpEvent::Transmit { packets: buf });
            }

            // Don't progress to move data between association and endpoint until we have an
            // association we want to drive forward.
            if !self.state.propagate_endpoint_to_assoc() {
                return None;
            }

            let assoc = self.assoc.as_mut()?;

            while let Some(e) = assoc.poll() {
                if let Event::Connected = e {
                    assoc.set_max_send_message_size(self.remote_max_message_size);
                    set_state(&mut self.state, RtcSctpState::Established);
                    continue 'restart;
                }

                if let Event::AssociationLost { ref reason } = e {
                    debug!("Association lost, reason: {}", reason);
                    // No reset can complete on a dead association.
                    self.reset_pending.clear();
                    self.reset_complete.clear();
                    return Some(SctpEvent::AssociationLost);
                }

                if let Event::Stream(se) = e {
                    match se {
                        StreamEvent::Readable { id } | StreamEvent::Writable { id } => {
                            stream_entry(
                                &mut self.entries,
                                id,
                                StreamEntryState::AwaitConfig,
                                "readable/writable",
                            );
                        }
                        StreamEvent::Finished { id } | StreamEvent::Stopped { id, .. } => {
                            // sctp-proto unregistered it when a reset arrived.
                            // Id reuse is signalled separately by StreamEvent::ResetComplete.
                            //
                            // sctp-proto arms that completion here, so from now on a reset
                            // is outstanding for the id even if we never closed it locally.
                            self.reset_pending.insert(id);

                            // Only a live entry has anything to drop. Closed entries and
                            // missing ones already went through Close, and an AwaitOpen
                            // entry is a new incarnation waiting on the same id, it must
                            // not be killed by the old pending teardown.
                            if let Some(entry) = entry_by_id_mut(&mut self.entries, id) {
                                if entry.state != StreamEntryState::Closed
                                    && entry.state != StreamEntryState::AwaitOpen
                                {
                                    debug!("Stream {} finished", id);
                                    entry.do_close = true;
                                }
                            }
                        }
                        StreamEvent::ResetComplete { id } => {
                            // The reset handshake for this id has fully completed.
                            debug!("Stream {} reset complete", id);
                            self.reset_pending.remove(&id);
                            self.reset_complete.push_back(id);
                        }
                        StreamEvent::BufferedAmountLow { id } => {
                            return Some(SctpEvent::BufferedAmountLow { id });
                        }
                        _ => {}
                    }
                }
            }

            // Must wait for association state to be established before opening streams.
            if self.state != RtcSctpState::Established {
                return None;
            }

            for entry in self.entries.iter_mut() {
                let want_open = entry.state == StreamEntryState::AwaitOpen;

                if want_open {
                    debug!("Open stream {}", entry.id);
                    match assoc.open_stream(entry.id, PayloadProtocolIdentifier::Unknown) {
                        Ok(mut s) => {
                            entry.open_deadline = None;

                            if !entry.configure_reliability(&mut s) {
                                entry.set_state(StreamEntryState::Closed);
                                let stream_id = entry.id;
                                let reset_pending = self.sctp_propagate_close(stream_id);
                                return Some(SctpEvent::Close {
                                    id: stream_id,
                                    reset_pending,
                                });
                            }

                            let config = entry.config.as_ref().expect("config if AwaitOpen");
                            let in_band = config.negotiated.is_none();

                            if in_band {
                                let dcep: DcepOpen = config.into();
                                let mut buf = vec![0; 1500];
                                let n = dcep.marshal_to(&mut buf);
                                buf.truncate(n);

                                match s.write_with_ppi(&buf, PayloadProtocolIdentifier::Dcep) {
                                    Ok(l) => {
                                        assert!(n == l);
                                        entry.set_state(StreamEntryState::AwaitDcepAck);

                                        // Start over with polling, since we might have caused
                                        // some network traffic by writing the DcepOpen.
                                        continue 'restart;
                                    }
                                    Err(e) => {
                                        warn!(
                                            "Failed to write DCEP open on stream {}: {:?}",
                                            entry.id, e
                                        );
                                        entry.do_close = true;
                                        entry.set_state(StreamEntryState::Closed);
                                        let stream_id = entry.id;
                                        let reset_pending = self.sctp_propagate_close(stream_id);
                                        return Some(SctpEvent::Close {
                                            id: stream_id,
                                            reset_pending,
                                        });
                                    }
                                }
                            }

                            // Continuing means we are opening the stream out-of-band.
                        }
                        Err(
                            e @ (ProtoError::ErrStreamAlreadyExist
                            | ProtoError::ErrStreamResetPending),
                        ) => {
                            let config = entry.config.as_ref().expect("config if AwaitOpen");
                            let in_band = config.negotiated.is_none();

                            // ErrStreamAlreadyExist has two causes:
                            // - the remote created it by sending on it first
                            // - a previous incarnation of the id is still registered,
                            //   reset handshake hasn't finished
                            let stale_incarnation = matches!(e, ProtoError::ErrStreamAlreadyExist)
                                && assoc
                                    .stream(entry.id)
                                    .map(|s| !s.is_readable() && !s.is_writable())
                                    .unwrap_or(false);

                            if in_band
                                || stale_incarnation
                                || matches!(e, ProtoError::ErrStreamResetPending)
                            {
                                // RFC 6525 reset handshake for a previous
                                // incarnation of this stream id hasn't finished yet
                                //
                                // - AlreadyExist clears when the remote's reciprocal reset arrives
                                // - ResetPending when the RECONFIG-RESPONSE arrives (silently)
                                //
                                // In both cases,  stay in AwaitOpen and retry until past the deadline.
                                let deadline = *entry
                                    .open_deadline
                                    .get_or_insert(self.last_now + STREAM_OPEN_TIMEOUT);

                                if self.last_now < deadline {
                                    debug!(
                                        "Stream {} open blocked ({:?}), will retry",
                                        entry.id, e
                                    );
                                    continue;
                                }

                                debug!("Opening stream {} failed after retries: {:?}", entry.id, e);
                                entry.do_close = true;
                                entry.set_state(StreamEntryState::Closed);
                                let stream_id = entry.id;
                                let reset_pending = self.sctp_propagate_close(stream_id);
                                return Some(SctpEvent::Close {
                                    id: stream_id,
                                    reset_pending,
                                });
                            }

                            // Continuing means we are adopting the live out-of-band stream the
                            // remote created. It skipped the Ok branch above, so reliability
                            // params haven't been applied to it yet.
                            let mut stream = assoc.stream(entry.id).expect("stream that exists");
                            if !entry.configure_reliability(&mut stream) {
                                entry.set_state(StreamEntryState::Closed);
                                let stream_id = entry.id;
                                let reset_pending = self.sctp_propagate_close(stream_id);
                                return Some(SctpEvent::Close {
                                    id: stream_id,
                                    reset_pending,
                                });
                            }
                        }
                        Err(e) => {
                            warn!("Opening stream {} failed: {:?}", entry.id, e);
                            entry.do_close = true;
                            entry.set_state(StreamEntryState::Closed);
                            let stream_id = entry.id;
                            let reset_pending = self.sctp_propagate_close(stream_id);
                            return Some(SctpEvent::Close {
                                id: stream_id,
                                reset_pending,
                            });
                        }
                    }

                    // Consider out-of-band stream open.
                    let config = entry.config.as_ref().expect("config if AwaitOpen");
                    let in_band = config.negotiated.is_none();
                    assert!(!in_band);

                    let label = config.label.clone();
                    entry.set_state(StreamEntryState::Open);

                    return Some(SctpEvent::Open {
                        id: entry.id,
                        label,
                    });
                }

                if entry.do_close && entry.state != StreamEntryState::Closed {
                    entry.set_state(StreamEntryState::Closed);
                    let stream_id = entry.id;
                    let reset_pending = self.sctp_propagate_close(stream_id);
                    return Some(SctpEvent::Close {
                        id: stream_id,
                        reset_pending,
                    });
                }

                let mut stream = match assoc.stream(entry.id) {
                    Ok(v) => v,
                    Err(e) => {
                        // This is expected on browser refresh or similar abrupt shutdown.
                        debug!("Getting stream {} failed: {:?}", entry.id, e);
                        entry.do_close = true;
                        continue 'restart;
                    }
                };

                // Propagate the desired buffered threshold.
                // The idea is to only do this if the user has changed the value for it without
                // incurring the cost of looking up the currently confifured value.
                if let BufferedThresholdConfig::Desired(x) = entry.buffered_threshold {
                    if let Err(e) = stream.set_buffered_amount_low_threshold(x) {
                        debug!("Setting buffered_amount_low_threshold failed: {:?}", e);
                        entry.do_close = true;
                        entry.buffered_threshold = BufferedThresholdConfig::Unconfigured;
                        continue 'restart;
                    }

                    entry.buffered_threshold = BufferedThresholdConfig::Configured(x);
                }
                match stream_read_data(&mut stream) {
                    Ok(Some((buf, ppi))) => {
                        if ppi != PayloadProtocolIdentifier::Dcep {
                            // This is the normal path for incoming data.
                            let buf = ppi_adjust_buf(buf, ppi);
                            let binary = matches!(
                                ppi,
                                PayloadProtocolIdentifier::Binary
                                    | PayloadProtocolIdentifier::BinaryEmpty
                            );
                            return Some(SctpEvent::Data {
                                id: entry.id,
                                binary,
                                data: buf,
                            });
                        }

                        // It's Dcep, either a DcepOpen or DcepAck.
                        match entry.state {
                            // We are in AwaitConfig state which means we are either going to get it via
                            // the DcepOpen, or by an out-of-band configuration via open_stream.
                            // This indicates we are doing in-band.
                            StreamEntryState::AwaitConfig => {
                                let dcep: DcepOpen = match buf.as_slice().try_into() {
                                    Ok(v) => v,
                                    Err(e) => {
                                        warn!("Failed to read incoming DCEP {}: {:?}", entry.id, e);
                                        entry.do_close = true;
                                        continue 'restart;
                                    }
                                };

                                if entry.config.is_none() {
                                    entry.config = Some((&dcep).into());
                                } else {
                                    warn!("Received DcepOpen for configured stream: {}", entry.id);
                                }

                                // Apply DcepOpen's reliability parameters to the sctp-proto stream.
                                // Without this, the DCEP-receiving side of an in-band channel sends
                                // with stream defaults: ordered and fully reliable.
                                if !entry.configure_reliability(&mut stream) {
                                    continue 'restart;
                                }

                                let mut obuf = [0];
                                DcepAck.marshal_to(&mut obuf);
                                match stream.write_with_ppi(&obuf, PayloadProtocolIdentifier::Dcep)
                                {
                                    Ok(l) => {
                                        assert!(obuf.len() == l);
                                        entry.set_state(StreamEntryState::Open);

                                        return Some(SctpEvent::Open {
                                            id: entry.id,
                                            label: dcep.label,
                                        });
                                    }
                                    Err(e) => {
                                        warn!(
                                            "Failed to write DCEP ack on stream {}: {:?}",
                                            entry.id, e
                                        );
                                        entry.do_close = true;
                                        entry.set_state(StreamEntryState::Closed);
                                        let stream_id = entry.id;
                                        let reset_pending = self.sctp_propagate_close(stream_id);
                                        return Some(SctpEvent::Close {
                                            id: stream_id,
                                            reset_pending,
                                        });
                                    }
                                }
                            }
                            StreamEntryState::AwaitDcepAck => {
                                let res: Result<DcepAck, _> = buf.as_slice().try_into();

                                if let Err(e) = res {
                                    warn!("Failed to read incoming DCEP ACK {}: {:?}", entry.id, e);
                                    entry.do_close = true;
                                    continue 'restart;
                                }

                                entry.set_state(StreamEntryState::Open);
                                let config = entry.config.as_ref().expect("config when DcepAck");

                                return Some(SctpEvent::Open {
                                    id: entry.id,
                                    label: config.label.clone(),
                                });
                            }
                            _ => {
                                warn!(
                                    "Stream {} in wrong state when receiving DCEP: {:?}",
                                    entry.id, entry.state
                                );
                                entry.do_close = true;
                                continue 'restart;
                            }
                        }
                    }
                    Ok(None) => continue,
                    Err(_) => {
                        entry.do_close = true;
                        continue 'restart;
                    }
                }
            }

            // Reset completions are reported last. Reaching here means the entry loop had
            // no `Close` left to emit, so the close for a released id has been delivered.
            if let Some(id) = self.reset_complete.pop_front() {
                return Some(SctpEvent::StreamResetComplete { id });
            }

            return None;
        }
    }

    pub fn poll_timeout(&mut self) -> Option<Instant> {
        let assoc_timeout = self.assoc.as_mut().and_then(|a| a.poll_timeout());

        // Wakeup backstop for entries whose open_stream() is being retried.
        //
        // Normally no wakeup is needed: a blocked open re-attempts on the
        // next do_poll(), and the packet that unblocks it (the peer's
        // RECONFIG-RESPONSE or reset) itself triggers handle_input() and that poll.
        //
        // Returning a wakeup while any entry retries guarantees the open
        // either resolves or fails within STREAM_OPEN_TIMEOUT.
        let retry_timeout = self
            .entries
            .iter()
            .any(|e| e.state == StreamEntryState::AwaitOpen && e.open_deadline.is_some())
            .then(|| self.last_now + STREAM_OPEN_RETRY_INTERVAL);

        match (assoc_timeout, retry_timeout) {
            (Some(a), Some(r)) => Some(a.min(r)),
            (a, r) => a.or(r),
        }
    }

    pub fn push_back_transmit(&mut self, data: VecDeque<Vec<u8>>) {
        trace!("Push back transmit: {}", data.len());
        assert!(self.pushed_back_transmit.is_none());
        self.pushed_back_transmit = Some(data);
    }

    fn poll_transmit(&mut self) -> Option<Transmit> {
        if let Some(t) = self.endpoint.poll_transmit() {
            return Some(t);
        }

        if let Some(t) = self.assoc.as_mut()?.poll_transmit(self.last_now) {
            return Some(t);
        }

        None
    }

    pub fn config(&self, sctp_stream_id: u16) -> Option<&ChannelConfig> {
        entry_by_id(&self.entries, sctp_stream_id).and_then(|s| s.config.as_ref())
    }

    /// Close the sctp stream to allow re-use of the same id.
    ///
    /// Returns whether a reset handshake is outstanding for `stream_id`.
    fn sctp_propagate_close(&mut self, stream_id: u16) -> bool {
        let did_reset = match self.assoc.as_mut().map(|assoc| assoc.stream(stream_id)) {
            // `close()` only fails on the reset, which needs an established
            // association. Closing a channel while the association is shutting down
            // therefore queues nothing and no completion will ever arrive.
            Some(Ok(mut stream)) => stream.close().is_ok(),
            // No stream to reset.
            _ => false,
        };

        if did_reset {
            self.reset_pending.insert(stream_id);
        }

        self.reset_pending.contains(&stream_id)
    }

    #[cfg(test)]
    pub(crate) fn remote_max_message_size(&self) -> u32 {
        self.remote_max_message_size
    }
}

fn transmit_to_vec(t: Transmit) -> Option<VecDeque<Vec<u8>>> {
    let Payload::RawEncode(v) = t.payload else {
        return None;
    };

    Some(v.into_iter().map(|b| b.to_vec()).collect())
}

fn set_state(current_state: &mut RtcSctpState, state: RtcSctpState) {
    if *current_state != state {
        debug!("{:?} => {:?}", current_state, state);
        *current_state = state;
    }
}

fn entry_index(entries: &[StreamEntry], id: u16) -> Result<usize, usize> {
    entries.binary_search_by_key(&id, |e| e.id)
}

fn entry_by_id(entries: &[StreamEntry], id: u16) -> Option<&StreamEntry> {
    entry_index(entries, id).ok().map(|i| &entries[i])
}

fn entry_by_id_mut(entries: &mut [StreamEntry], id: u16) -> Option<&mut StreamEntry> {
    entry_index(entries, id).ok().map(|i| &mut entries[i])
}

fn stream_entry<'a>(
    entries: &'a mut Vec<StreamEntry>,
    id: u16,
    initial_state: StreamEntryState,
    reason: &'static str,
) -> &'a mut StreamEntry {
    let idx = match entry_index(entries, id) {
        Ok(idx) => idx,
        Err(idx) => {
            debug!("New stream {} ({:?}): {}", id, initial_state, reason);
            entries.insert(
                idx,
                StreamEntry {
                    config: None,
                    state: initial_state,
                    id,
                    do_close: false,
                    open_deadline: None,
                    buffered_threshold: BufferedThresholdConfig::Unconfigured,
                },
            );
            idx
        }
    };

    &mut entries[idx]
}

fn stream_read_data(
    stream: &mut Stream,
) -> Result<Option<(Vec<u8>, PayloadProtocolIdentifier)>, SctpError> {
    let Some(chunks) = stream.read()? else {
        return Ok(None);
    };

    let n = chunks.len();
    let mut buf = vec![0; n];

    let l = chunks.read(&mut buf)?;
    assert!(l == n);

    use PayloadProtocolIdentifier::*;
    match chunks.ppi {
        Dcep | String | Binary => {} // keep as is
        StringEmpty | BinaryEmpty => buf.clear(),
        _ => {
            return Err(SctpError::Proto(ProtoError::Other(
                "Unknown PayloadProtocolIdentifier".into(),
            )));
        }
    }

    Ok(Some((buf, chunks.ppi)))
}

fn ppi_adjust_buf(mut buf: Vec<u8>, ppi: PayloadProtocolIdentifier) -> Vec<u8> {
    match ppi {
        PayloadProtocolIdentifier::StringEmpty | PayloadProtocolIdentifier::BinaryEmpty => {
            buf.clear();
            buf
        }
        _ => buf,
    }
}

impl fmt::Debug for SctpEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transmit { packets } => f
                .debug_struct("Transmit")
                .field("packets", &packets.len())
                .finish(),
            Self::Open { id, label } => f
                .debug_struct("Open")
                .field("id", id)
                .field("label", label)
                .finish(),
            Self::Close { id, reset_pending } => f
                .debug_struct("Close")
                .field("id", id)
                .field("reset_pending", reset_pending)
                .finish(),
            Self::StreamResetComplete { id } => f
                .debug_struct("StreamResetComplete")
                .field("id", id)
                .finish(),
            Self::Data { id, binary, data } => f
                .debug_struct("Data")
                .field("id", id)
                .field("binary", binary)
                .field("data", &data.len())
                .finish(),
            Self::BufferedAmountLow { id } => {
                f.debug_struct("BufferedAmountLow").field("id", id).finish()
            }
            Self::AssociationLost => f.debug_struct("AssociationLost").finish(),
        }
    }
}

impl From<&ChannelConfig> for DcepOpen {
    fn from(v: &ChannelConfig) -> Self {
        let (channel_type, reliability_parameter) = (&v.reliability).into();
        DcepOpen {
            unordered: !v.ordered,
            channel_type,
            reliability_parameter,
            priority: 0,
            label: v.label.clone(),
            protocol: v.protocol.clone(),
        }
    }
}

impl From<&Reliability> for (ReliabilityType, u32) {
    fn from(v: &Reliability) -> Self {
        match v {
            Reliability::Reliable => (ReliabilityType::Reliable, 0),
            Reliability::MaxPacketLifetime { lifetime } => {
                (ReliabilityType::Timed, *lifetime as u32)
            }
            Reliability::MaxRetransmits { retransmits } => {
                (ReliabilityType::Rexmit, *retransmits as u32)
            }
        }
    }
}

impl From<&DcepOpen> for ChannelConfig {
    fn from(v: &DcepOpen) -> Self {
        ChannelConfig {
            label: v.label.clone(),
            ordered: !v.unordered,
            reliability: (v.channel_type, v.reliability_parameter).into(),
            negotiated: None,
            protocol: v.protocol.clone(),
        }
    }
}

impl From<(ReliabilityType, u32)> for Reliability {
    fn from((r, p): (ReliabilityType, u32)) -> Self {
        match r {
            ReliabilityType::Reliable => Reliability::Reliable,
            ReliabilityType::Rexmit => Reliability::MaxRetransmits {
                retransmits: p as u16,
            },
            ReliabilityType::Timed => Reliability::MaxPacketLifetime { lifetime: p as u16 },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::poll::Readiness;

    #[test]
    fn readiness_uninitialized_sctp_timeout_does_not_wake() {
        let now = Instant::now();
        let mut sctp = RtcSctp::new(DATAGRAM_MTU_TARGET, LOCAL_MAX_MESSAGE_SIZE);
        let mut readiness = Readiness::idle();
        sctp.handle_timeout(now, &mut readiness.wake());
        assert!(!readiness.wants_events());
        assert!(!readiness.wants_timeout());
        assert!(sctp.poll_timeout().is_none());
        assert!(sctp.poll().is_none());
    }

    #[test]
    fn readiness_passive_sctp_timeout_does_not_wake_events() {
        let now = Instant::now();
        let mut sctp = RtcSctp::new(DATAGRAM_MTU_TARGET, LOCAL_MAX_MESSAGE_SIZE);
        sctp.init(false, now, None, None).unwrap();
        assert!(sctp.assoc.is_none());
        let mut readiness = Readiness::idle();
        sctp.handle_timeout(now, &mut readiness.wake());
        assert!(!readiness.wants_events());
        assert!(readiness.wants_timeout());
        assert!(sctp.poll().is_none());
    }

    #[test]
    fn readiness_sctp_retransmission_keeps_both_signals() {
        let now = Instant::now();
        let mut sctp = RtcSctp::new(DATAGRAM_MTU_TARGET, LOCAL_MAX_MESSAGE_SIZE);
        sctp.init(true, now, None, None).unwrap();
        while sctp.poll().is_some() {}
        let deadline = sctp.poll_timeout().unwrap();
        let mut readiness = Readiness::idle();
        sctp.handle_timeout(deadline, &mut readiness.wake());
        assert!(readiness.wants_events());
        assert!(readiness.wants_timeout());
        assert!(matches!(sctp.poll(), Some(SctpEvent::Transmit { .. })));
        assert!(sctp.poll_timeout().unwrap() > deadline);
    }

    /// Insert an entry directly, keeping `entries` sorted by id.
    fn insert_entry(entries: &mut Vec<StreamEntry>, entry: StreamEntry) {
        let idx = entry_index(entries, entry.id).expect_err("entry should not already exist");
        entries.insert(idx, entry);
    }
    use str0m_proto::DATAGRAM_MTU_TARGET;

    #[test]
    fn partial_snap_init_requires_both_chunks() {
        let now = Instant::now();
        let mut sctp = RtcSctp::new(DATAGRAM_MTU_TARGET, LOCAL_MAX_MESSAGE_SIZE);
        let mut init_data = SctpInitData::new();

        init_data.local_init_chunk().unwrap();

        let err = sctp.init(true, now, Some(init_data), None).unwrap_err();
        assert!(
            err.to_string()
                .contains("SNAP requires both local and remote SCTP INIT chunks")
        );
    }

    #[test]
    fn malformed_remote_snap_does_not_disable_local_opt_in() {
        let mut sctp = RtcSctp::new(DATAGRAM_MTU_TARGET, LOCAL_MAX_MESSAGE_SIZE);
        sctp.enable_snap();

        assert!(!sctp.set_remote_snap_init_string("!!!not-valid-base64!!!"));
        assert!(sctp.snap_enabled());
        assert!(sctp.ensure_local_snap_init());
        assert!(sctp.local_sctp_init_for_sdp().is_some());
    }

    /// Helper to connect a client and server RtcSctp pair to Established state.
    fn connect_client_server() -> (RtcSctp, RtcSctp) {
        let now = Instant::now();
        let mut client = RtcSctp::new(DATAGRAM_MTU_TARGET, LOCAL_MAX_MESSAGE_SIZE);
        let mut server = RtcSctp::new(DATAGRAM_MTU_TARGET, LOCAL_MAX_MESSAGE_SIZE);

        client.init(true, now, None, None).unwrap();
        server.init(false, now, None, None).unwrap();

        // Exchange packets until both are Established.
        for _ in 0..20 {
            // Drain client transmits -> feed to server
            while let Some(t) = client.poll_transmit() {
                if let Some(bufs) = transmit_to_vec(t) {
                    for buf in bufs {
                        server.handle_input(now, &buf);
                    }
                }
            }
            // Process server events
            while let Some(e) = server.do_poll() {
                if let SctpEvent::Transmit { packets } = e {
                    for buf in packets {
                        client.handle_input(now, &buf);
                    }
                }
            }

            // Drain server transmits -> feed to client
            while let Some(t) = server.poll_transmit() {
                if let Some(bufs) = transmit_to_vec(t) {
                    for buf in bufs {
                        client.handle_input(now, &buf);
                    }
                }
            }
            // Process client events
            while let Some(e) = client.do_poll() {
                if let SctpEvent::Transmit { packets } = e {
                    for buf in packets {
                        server.handle_input(now, &buf);
                    }
                }
            }

            // Check if both established
            if client.state == RtcSctpState::Established
                && server.state == RtcSctpState::Established
            {
                break;
            }
        }

        assert_eq!(client.state, RtcSctpState::Established);
        assert_eq!(server.state, RtcSctpState::Established);

        (client, server)
    }

    fn assert_close_before_idle(sctp: &mut RtcSctp, id: u16) {
        for _ in 0..100 {
            match sctp.poll() {
                Some(SctpEvent::Close {
                    id: closed,
                    reset_pending,
                }) => {
                    assert_eq!(closed, id);
                    assert!(reset_pending, "the malformed stream must start a reset");
                    return;
                }
                Some(_) => {}
                None => panic!("SCTP reported idle before draining the deferred close"),
            }
        }
        panic!("SCTP did not finish draining");
    }

    fn transfer_transmits(from: &mut RtcSctp, to: &mut RtcSctp) {
        while let Some(t) = from.poll_transmit() {
            if let Some(packets) = transmit_to_vec(t) {
                for packet in packets {
                    to.handle_input(to.last_now, &packet);
                }
            }
        }
    }

    #[test]
    fn malformed_dcep_open_closes_before_idle() {
        let (mut client, mut server) = connect_client_server();
        // Send an actual SCTP DATA chunk with an invalid DCEP OPEN body.
        client
            .assoc
            .as_mut()
            .unwrap()
            .open_stream(0, PayloadProtocolIdentifier::Dcep)
            .unwrap()
            .write_with_ppi(&[0xff], PayloadProtocolIdentifier::Dcep)
            .unwrap();
        transfer_transmits(&mut client, &mut server);
        assert_close_before_idle(&mut server, 0);
    }

    #[test]
    fn unexpected_dcep_on_open_stream_closes_before_idle() {
        let (mut client, mut server) = connect_client_server();
        let config = ChannelConfig {
            negotiated: Some(0),
            ..Default::default()
        };
        client.open_stream(0, config.clone());
        server.open_stream(0, config);
        assert!(matches!(client.poll(), Some(SctpEvent::Open { id: 0, .. })));
        assert!(matches!(server.poll(), Some(SctpEvent::Open { id: 0, .. })));
        client
            .assoc
            .as_mut()
            .unwrap()
            .stream(0)
            .unwrap()
            .write_with_ppi(&[2], PayloadProtocolIdentifier::Dcep)
            .unwrap();
        transfer_transmits(&mut client, &mut server);
        assert_close_before_idle(&mut server, 0);
    }

    #[test]
    fn malformed_dcep_ack_closes_before_idle() {
        let (mut client, mut server) = connect_client_server();
        client.open_stream(0, ChannelConfig::default());
        while let Some(event) = client.poll() {
            if let SctpEvent::Transmit { packets } = event {
                for packet in packets {
                    server.handle_input(server.last_now, &packet);
                }
            }
        }
        assert_eq!(
            entry_by_id_mut(&mut client.entries, 0).unwrap().state,
            StreamEntryState::AwaitDcepAck
        );
        // Bypass the server's DCEP handler to return a malformed ACK over SCTP.
        server
            .assoc
            .as_mut()
            .unwrap()
            .stream(0)
            .unwrap()
            .write_with_ppi(&[0xff], PayloadProtocolIdentifier::Dcep)
            .unwrap();
        transfer_transmits(&mut server, &mut client);
        assert_close_before_idle(&mut client, 0);
    }

    /// A stream the remote opened can be gone from the association by the time the
    /// application configures it.
    ///
    /// `StreamEntryState::AwaitConfig` is set when a `Readable`/`Writable` event
    /// arrives
    /// for an id we have no config for. If the peer then resets that stream, sctp-proto
    /// unregisters it during `handle_input()`, while our entry stays `AwaitConfig` until
    /// the reset event is drained in a later `do_poll()`.
    #[test]
    fn open_stream_out_of_band_after_remote_reset() {
        let (mut client, _server) = connect_client_server();

        let stream_id: u16 = 3;
        assert!(
            client.assoc.as_mut().unwrap().stream(stream_id).is_err(),
            "the association must not have this stream for the test to mean anything"
        );

        insert_entry(
            &mut client.entries,
            StreamEntry {
                config: None,
                state: StreamEntryState::AwaitConfig,
                id: stream_id,
                do_close: false,
                open_deadline: None,
                buffered_threshold: BufferedThresholdConfig::Unconfigured,
            },
        );

        client.open_stream(
            stream_id,
            ChannelConfig {
                label: "negotiated".to_string(),
                ordered: true,
                reliability: Reliability::Reliable,
                negotiated: Some(stream_id),
                protocol: String::new(),
            },
        );

        let entry = client
            .entries
            .iter()
            .find(|e| e.id == stream_id)
            .expect("entry to still exist");

        assert!(
            entry.do_close,
            "a stream that vanished from the association should be marked for close"
        );
        assert_ne!(
            entry.state,
            StreamEntryState::Open,
            "must not go Open when there is no underlying stream"
        );
    }

    /// Regression test: when `assoc.open_stream()` returns `ErrStreamAlreadyExist`
    /// for an in-band (DCEP) data channel, the entry must eventually transition to
    /// Closed and emit `SctpEvent::Close`. The error is transient (a reset
    /// handshake could clear it), so the entry retries first — but bounded by
    /// `STREAM_OPEN_TIMEOUT`, not the infinite loop this once was.
    #[test]
    fn err_stream_already_exist_in_band_returns_close() {
        let (mut client, _server) = connect_client_server();

        let stream_id: u16 = 0;

        // Pre-create the stream in the association so the next open_stream() with the
        // same ID will return ErrStreamAlreadyExist.
        let assoc = client.assoc.as_mut().unwrap();
        assoc
            .open_stream(stream_id, PayloadProtocolIdentifier::Unknown)
            .expect("first open_stream should succeed");

        // Manually add an entry in AwaitOpen state with in-band config (negotiated: None).
        // This simulates a locally-initiated in-band channel whose stream ID conflicts
        // with one already opened by the remote peer.
        insert_entry(
            &mut client.entries,
            StreamEntry {
                config: Some(ChannelConfig {
                    label: "test".to_string(),
                    ordered: true,
                    reliability: Reliability::Reliable,
                    negotiated: None, // in-band
                    protocol: String::new(),
                }),
                state: StreamEntryState::AwaitOpen,
                id: stream_id,
                do_close: false,
                open_deadline: None,
                buffered_threshold: BufferedThresholdConfig::Unconfigured,
            },
        );

        // The first poll retries instead of failing (arms the deadline).
        let event = client.do_poll();
        assert!(
            event.is_none(),
            "expected retry (no event) for stream {stream_id}, got {event:?}"
        );

        // Past the deadline the open gives up and closes.
        let later = Instant::now() + STREAM_OPEN_TIMEOUT + Duration::from_secs(1);
        let mut readiness = crate::poll::Readiness::armed();
        client.handle_timeout(later, &mut readiness.wake());
        let event = client.do_poll();

        assert!(
            matches!(&event, Some(SctpEvent::Close { id, .. }) if *id == stream_id),
            "expected SctpEvent::Close for stream {stream_id}, got {event:?}"
        );

        // Verify entry transitioned to Closed.
        let entry = entry_by_id(&client.entries, stream_id).unwrap();
        assert_eq!(entry.state, StreamEntryState::Closed);
    }

    #[test]
    fn negotiated_reuse_does_not_report_open_for_old_closed_stream() {
        let (mut client, _server) = connect_client_server();
        let stream_id = 0;

        let assoc = client.assoc.as_mut().unwrap();
        let mut old = assoc
            .open_stream(stream_id, PayloadProtocolIdentifier::Unknown)
            .expect("old stream should open");
        old.close().expect("old stream should start closing");

        insert_entry(
            &mut client.entries,
            StreamEntry {
                config: Some(ChannelConfig {
                    label: "replacement".to_string(),
                    ordered: true,
                    reliability: Reliability::Reliable,
                    negotiated: Some(stream_id),
                    protocol: String::new(),
                }),
                state: StreamEntryState::AwaitOpen,
                id: stream_id,
                do_close: false,
                open_deadline: None,
                buffered_threshold: BufferedThresholdConfig::Unconfigured,
            },
        );

        for _ in 0..10 {
            match client.do_poll() {
                Some(SctpEvent::Open { id, .. }) if id == stream_id => {
                    panic!("replacement was reported open while the old stream still exists")
                }
                Some(_) => {}
                None => break,
            }
        }
    }

    #[test]
    fn close_is_reported_before_reset_complete_when_events_are_batched() {
        let now = Instant::now();
        let (mut client, mut server) = connect_client_server();
        let stream_id = 0;
        let config = ChannelConfig {
            label: "batched-close".to_string(),
            negotiated: Some(stream_id),
            ..Default::default()
        };

        client.open_stream(stream_id, config.clone());
        server.open_stream(stream_id, config);
        assert!(matches!(
            client.do_poll(),
            Some(SctpEvent::Open { id, .. }) if id == stream_id
        ));
        assert!(matches!(
            server.do_poll(),
            Some(SctpEvent::Open { id, .. }) if id == stream_id
        ));

        client.close_stream(stream_id);

        // Move all reset packets in both directions without polling client
        // application events. This batches Finished and ResetComplete in the
        // association event queue, which is valid for a sans-I/O caller.
        for _ in 0..4 {
            while let Some(transmit) = client.poll_transmit() {
                for packet in transmit_to_vec(transmit).unwrap() {
                    server.handle_input(now, &packet);
                }
            }
            while let Some(transmit) = server.poll_transmit() {
                for packet in transmit_to_vec(transmit).unwrap() {
                    client.handle_input(now, &packet);
                }
            }
        }

        for _ in 0..10 {
            match client.do_poll() {
                Some(SctpEvent::Close { id, .. }) if id == stream_id => return,
                Some(SctpEvent::StreamResetComplete { id }) if id == stream_id => {
                    panic!("ResetComplete was reported before Close")
                }
                Some(_) => {}
                None => break,
            }
        }

        panic!("stream close was not reported");
    }

    /// Regression test: the DCEP-receiving side of an in-band channel must apply
    /// the DcepOpen reliability parameters (here: unordered) to its sctp-proto
    /// stream, instead of sending with stream defaults (ordered, fully reliable).
    #[test]
    fn dcep_receiver_applies_reliability_params() {
        let now = Instant::now();
        let (mut client, mut server) = connect_client_server();

        // Drain `from`, feeding packets to `to` and returning the raw bytes.
        let pump = |from: &mut RtcSctp, to: &mut RtcSctp| {
            let mut wire = vec![];
            while let Some(e) = from.do_poll() {
                if let SctpEvent::Transmit { packets } = e {
                    for p in packets {
                        to.handle_input(now, &p);
                        wire.extend(p);
                    }
                }
            }
            wire
        };

        // Client opens an unordered in-band (DCEP) channel; once the handshake is
        // pumped through, the server (DCEP receiver) sends data back.
        client.open_stream(
            0,
            ChannelConfig {
                ordered: false,
                ..Default::default()
            },
        );
        pump(&mut client, &mut server); // DcepOpen
        pump(&mut server, &mut client); // DcepAck, server side now open
        let payload = b"from dcep receiver";
        server.write(0, true, payload).unwrap();

        // The 16-byte DATA chunk header puts the flags byte 15 bytes before the
        // payload it carries. 0x04 is the U (unordered) flag.
        let wire = pump(&mut server, &mut client);
        let pos = wire
            .windows(payload.len())
            .position(|w| w == payload)
            .unwrap();
        assert!(
            wire[pos - 15] & 0x04 != 0,
            "DCEP receiver should send unordered"
        );
    }

    #[test]
    fn max_payload_size_matches_mtu_minus_overhead() {
        let overhead = crate::io::MAX_DTLS_OVERHEAD + SCTP_OVERHEAD;

        let default_sctp = RtcSctp::new(DATAGRAM_MTU_TARGET, LOCAL_MAX_MESSAGE_SIZE);
        assert_eq!(
            default_sctp.max_payload_size(),
            DATAGRAM_MTU_TARGET - overhead
        );

        let small_sctp = RtcSctp::new(900, LOCAL_MAX_MESSAGE_SIZE);
        assert_eq!(small_sctp.max_payload_size(), 900 - overhead);
    }
}
