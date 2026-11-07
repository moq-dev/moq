use crate::util::{AssociationIdGenerator, RandomAssociationIdGenerator};

use alloc::boxed::Box;
use alloc::sync::Arc;
use bytes::Bytes;
use core::fmt;

/// MTU for inbound packet (from DTLS)
pub(crate) const RECEIVE_MTU: usize = 8192;
/// initial MTU for outgoing packets (to DTLS)
pub(crate) const INITIAL_MTU: u32 = 1228;
pub(crate) const INITIAL_RECV_BUF_SIZE: u32 = 1024 * 1024;
pub(crate) const COMMON_HEADER_SIZE: u32 = 12;
pub(crate) const DATA_CHUNK_HEADER_SIZE: u32 = 16;
pub(crate) const DEFAULT_MAX_MESSAGE_SIZE: u32 = 65536;

// Default RTO values in milliseconds (RFC 4960)
pub(crate) const RTO_INITIAL: u64 = 3000;
pub(crate) const RTO_MIN: u64 = 1000;
pub(crate) const RTO_MAX: u64 = 60000;

// Default max retransmit value (RFC 4960 Section 15)
const DEFAULT_MAX_INIT_RETRANS: usize = 8;

/// Config collects the arguments to create_association construction into
/// a single structure
#[derive(Debug)]
pub struct TransportConfig {
    max_receive_buffer_size: u32,
    max_num_outbound_streams: u16,
    max_num_inbound_streams: u16,

    /// Maximum message size we will SEND (respects remote's advertised limit)
    /// Can be updated after association creation via set_max_send_message_size()
    max_send_message_size: u32,

    /// Maximum message size we will RECEIVE (what we advertise in SDP)
    /// Enforced during reassembly - messages exceeding this are rejected
    max_receive_message_size: u32,

    /// Maximum number of retransmissions for INIT chunks during handshake.
    /// Set to `None` for unlimited retries (recommended for WebRTC).
    /// Default: Some(8)
    max_init_retransmits: Option<usize>,

    /// Maximum number of retransmissions for DATA chunks.
    /// Set to `None` for unlimited retries (recommended for WebRTC).
    /// Default: None (unlimited)
    max_data_retransmits: Option<usize>,

    /// Initial retransmission timeout in milliseconds.
    /// Default: 3000
    rto_initial_ms: u64,

    /// Minimum retransmission timeout in milliseconds.
    /// Default: 1000
    rto_min_ms: u64,

    /// Maximum retransmission timeout in milliseconds.
    /// Default: 60000
    rto_max_ms: u64,

    /// SCTP source port when this endpoint initiates the association.
    ///
    /// Default: [`DEFAULT_SCTP_PORT`]
    local_port: u16,

    /// SCTP destination port when this endpoint initiates the association.
    ///
    /// Default: [`DEFAULT_SCTP_PORT`]
    remote_port: u16,
}

impl Default for TransportConfig {
    fn default() -> Self {
        TransportConfig {
            max_receive_buffer_size: INITIAL_RECV_BUF_SIZE,
            max_send_message_size: DEFAULT_MAX_MESSAGE_SIZE,
            max_receive_message_size: DEFAULT_MAX_MESSAGE_SIZE,
            max_num_outbound_streams: u16::MAX,
            max_num_inbound_streams: u16::MAX,
            max_init_retransmits: Some(DEFAULT_MAX_INIT_RETRANS),
            max_data_retransmits: None,
            rto_initial_ms: RTO_INITIAL,
            rto_min_ms: RTO_MIN,
            rto_max_ms: RTO_MAX,
            local_port: DEFAULT_SCTP_PORT,
            remote_port: DEFAULT_SCTP_PORT,
        }
    }
}

impl TransportConfig {
    /// Set the receive storage budget across all streams, including reset-deferred DATA.
    ///
    /// Reads deliver complete messages only. If this budget is exhausted with no
    /// readable message and no TSN gaps that can still be filled, the association
    /// fails with [`crate::AssociationError::ReceiveBufferExhausted`] and sends an
    /// Out Of Resource ABORT. No partial message is delivered. This can happen
    /// when a message exceeds the budget or multiple incomplete messages share it.
    /// Messages retained on stopped read halves also consume this budget until reset.
    /// Complete messages on readable streams apply ordinary receive-window backpressure.
    /// The maximum receive-message size remains an independent limit.
    pub fn with_max_receive_buffer_size(mut self, value: u32) -> Self {
        self.max_receive_buffer_size = value;
        self
    }

    pub fn with_max_send_message_size(mut self, value: u32) -> Self {
        self.max_send_message_size = value;
        self
    }

    /// Set maximum size of messages we will accept
    pub fn with_max_receive_message_size(mut self, value: u32) -> Self {
        self.max_receive_message_size = value;
        self
    }

    #[deprecated(note = "Use with_max_send_message_size instead")]
    pub fn with_max_message_size(self, value: u32) -> Self {
        self.with_max_send_message_size(value)
    }

    pub fn with_max_num_outbound_streams(mut self, value: u16) -> Self {
        self.max_num_outbound_streams = value;
        self
    }

    pub fn with_max_num_inbound_streams(mut self, value: u16) -> Self {
        self.max_num_inbound_streams = value;
        self
    }

    pub(crate) fn max_receive_buffer_size(&self) -> u32 {
        self.max_receive_buffer_size
    }

    pub(crate) fn max_send_message_size(&self) -> u32 {
        self.max_send_message_size
    }

    pub(crate) fn max_receive_message_size(&self) -> u32 {
        self.max_receive_message_size
    }

    pub(crate) fn max_num_outbound_streams(&self) -> u16 {
        self.max_num_outbound_streams
    }

    pub(crate) fn max_num_inbound_streams(&self) -> u16 {
        self.max_num_inbound_streams
    }

    /// Set maximum INIT retransmissions. `None` means unlimited.
    pub fn with_max_init_retransmits(mut self, value: Option<usize>) -> Self {
        self.max_init_retransmits = value;
        self
    }

    /// Set maximum DATA retransmissions. `None` means unlimited.
    pub fn with_max_data_retransmits(mut self, value: Option<usize>) -> Self {
        self.max_data_retransmits = value;
        self
    }

    /// Set initial RTO in milliseconds.
    pub fn with_rto_initial_ms(mut self, value: u64) -> Self {
        self.rto_initial_ms = value;
        self
    }

    /// Set minimum RTO in milliseconds.
    pub fn with_rto_min_ms(mut self, value: u64) -> Self {
        self.rto_min_ms = value;
        self
    }

    /// Set maximum RTO in milliseconds.
    pub fn with_rto_max_ms(mut self, value: u64) -> Self {
        self.rto_max_ms = value;
        self
    }

    /// Set the SCTP source port used when initiating an association.
    /// A zero port is rejected by [`crate::Endpoint::connect`].
    pub fn with_local_port(mut self, value: u16) -> Self {
        self.local_port = value;
        self
    }

    /// Set the SCTP destination port used when initiating an association.
    /// A zero port is rejected by [`crate::Endpoint::connect`].
    pub fn with_remote_port(mut self, value: u16) -> Self {
        self.remote_port = value;
        self
    }

    pub(crate) fn max_init_retransmits(&self) -> Option<usize> {
        self.max_init_retransmits
    }

    pub(crate) fn max_data_retransmits(&self) -> Option<usize> {
        self.max_data_retransmits
    }

    pub(crate) fn rto_initial_ms(&self) -> u64 {
        self.rto_initial_ms
    }

    pub(crate) fn rto_min_ms(&self) -> u64 {
        self.rto_min_ms
    }

    pub(crate) fn rto_max_ms(&self) -> u64 {
        self.rto_max_ms
    }

    pub(crate) fn local_port(&self) -> u16 {
        self.local_port
    }

    pub(crate) fn remote_port(&self) -> u16 {
        self.remote_port
    }
}

/// Global configuration for the endpoint, affecting all associations
///
/// Default values should be suitable for most internet applications.
#[derive(Clone)]
pub struct EndpointConfig {
    pub(crate) max_payload_size: u32,

    /// AID generator factory
    ///
    /// Create a aid generator for local aid in Endpoint struct
    pub(crate) aid_generator_factory:
        Arc<dyn Fn() -> Box<dyn AssociationIdGenerator> + Send + Sync>,
}

impl Default for EndpointConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl EndpointConfig {
    /// Create a default config
    pub fn new() -> Self {
        let aid_factory: fn() -> Box<dyn AssociationIdGenerator> =
            || Box::<RandomAssociationIdGenerator>::default();
        Self {
            max_payload_size: INITIAL_MTU - (COMMON_HEADER_SIZE + DATA_CHUNK_HEADER_SIZE),
            aid_generator_factory: Arc::new(aid_factory),
        }
    }

    /// Supply a custom Association ID generator factory
    ///
    /// Called once by each `Endpoint` constructed from this configuration to obtain the AID
    /// generator which will be used to generate the AIDs used for incoming packets on all
    /// associations involving that  `Endpoint`. A custom AID generator allows applications to embed
    /// information in local association IDs, e.g. to support stateless packet-level load balancers.
    ///
    /// `EndpointConfig::new()` applies a default random AID generator factory. This functions
    /// accepts any customized AID generator to reset AID generator factory that implements
    /// the `AssociationIdGenerator` trait.
    pub fn aid_generator<F: Fn() -> Box<dyn AssociationIdGenerator> + Send + Sync + 'static>(
        &mut self,
        factory: F,
    ) -> &mut Self {
        self.aid_generator_factory = Arc::new(factory);
        self
    }

    /// Maximum payload size accepted from peers.
    ///
    /// The default is suitable for typical internet applications. Applications which expect to run
    /// on networks supporting Ethernet jumbo frames or similar should set this appropriately.
    pub fn max_payload_size(&mut self, value: u32) -> &mut Self {
        self.max_payload_size = value;
        self
    }

    /// Get the current value of `max_payload_size`
    ///
    /// While most parameters don't need to be readable, this must be exposed to allow higher-level
    /// layers to determine how large a receive buffer to allocate to
    /// support an externally-defined `EndpointConfig`.
    ///
    /// While `get_` accessors are typically unidiomatic in Rust, we favor concision for setters,
    /// which will be used far more heavily.
    #[doc(hidden)]
    pub fn get_max_payload_size(&self) -> u32 {
        self.max_payload_size
    }
}

impl fmt::Debug for EndpointConfig {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.debug_struct("EndpointConfig")
            .field("max_payload_size", &self.max_payload_size)
            .field("aid_generator_factory", &"[ elided ]")
            .finish()
    }
}

/// Parameters governing incoming associations
///
/// Default values should be suitable for most internet applications.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Transport configuration to use for incoming associations
    pub transport: Arc<TransportConfig>,

    /// Maximum number of concurrent associations
    pub(crate) concurrent_associations: u32,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            transport: Arc::new(TransportConfig::default()),
            concurrent_associations: 100_000,
        }
    }
}

impl ServerConfig {
    /// Create a default config with a particular handshake token key
    pub fn new() -> Self {
        ServerConfig::default()
    }
}

/// Default SCTP source/destination port (conventional for WebRTC data channels).
pub const DEFAULT_SCTP_PORT: u16 = 5000;

/// Maximum allowed size (in bytes) of a serialized SNAP token (INIT chunk)
/// accepted via out-of-band negotiation. A typical token is well under
/// 100 bytes; this limit prevents accidentally feeding megabytes of
/// untrusted signaling data into the parser.
pub const MAX_SNAP_INIT_BYTES: usize = 2048;

/// Configuration for outgoing associations.
///
/// Default values should be suitable for most internet applications.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Transport configuration to use
    pub transport: Arc<TransportConfig>,
    /// Local SNAP token (INIT chunk) bytes.
    ///
    /// Generated via [`generate_snap_token`]. When both `local_sctp_init` and
    /// `remote_sctp_init` are set, the association skips the SCTP 4-way
    /// handshake (RFC 4960 Section 5.1) and immediately transitions to the
    /// ESTABLISHED state.
    ///
    /// If only one side is set (e.g. the peer does not support SNAP), the
    /// association falls back to the normal SCTP handshake.
    ///
    /// See [draft-hancke-tsvwg-snap-01](https://datatracker.ietf.org/doc/draft-hancke-tsvwg-snap/).
    pub(crate) local_sctp_init: Option<Bytes>,
    /// Remote SNAP token (INIT chunk) bytes.
    ///
    /// Received from the peer via a signaling channel (e.g., SDP `a=sctp-init`
    /// attribute). Must be provided together with `local_sctp_init` to enable
    /// SNAP.
    ///
    /// See [draft-hancke-tsvwg-snap-01](https://datatracker.ietf.org/doc/draft-hancke-tsvwg-snap/).
    pub(crate) remote_sctp_init: Option<Bytes>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        ClientConfig {
            transport: Arc::new(TransportConfig::default()),
            local_sctp_init: None,
            remote_sctp_init: None,
        }
    }
}

impl ClientConfig {
    /// Create a default config with a particular cryptographic config
    pub fn new() -> Self {
        ClientConfig::default()
    }

    /// Enable SNAP (SCTP Negotiation Acceleration Protocol).
    ///
    /// Both a local and remote SNAP token (INIT chunk) must be provided.
    /// The local token should be generated via [`generate_snap_token`] and
    /// exchanged with the remote peer through a signaling channel (e.g.,
    /// SDP `a=sctp-init` attribute). The remote token is the peer's
    /// corresponding bytes received via signaling.
    ///
    /// When both are set, the association skips the SCTP 4-way handshake
    /// (RFC 4960 Section 5.1) and immediately transitions to the ESTABLISHED
    /// state.
    ///
    /// **Note:** When using SNAP, **both** peers must call
    /// [`Endpoint::connect`](crate::Endpoint::connect) — there is no
    /// server-side SNAP via [`Endpoint::handle`](crate::Endpoint::handle).
    ///
    /// See [draft-hancke-tsvwg-snap-01](https://datatracker.ietf.org/doc/draft-hancke-tsvwg-snap/).
    pub fn with_snap(mut self, local_sctp_init: Bytes, remote_sctp_init: Bytes) -> Self {
        self.local_sctp_init = Some(local_sctp_init);
        self.remote_sctp_init = Some(remote_sctp_init);
        self
    }
}

/// Generate a SNAP token (INIT chunk) for out-of-band negotiation.
///
/// Creates a serialized SCTP INIT **chunk** (not a full SCTP packet — no
/// common header or IP/UDP framing) with random `initiate_tag` and
/// `initial_tsn` values, using the receiver window from the provided
/// [`TransportConfig`]. Stream counts are set to `u16::MAX` so that the
/// actual limit is determined by the peer's offer during negotiation.
///
/// The returned bytes are suitable for exchange via a signaling channel
/// (e.g., SDP `a=sctp-init`) as described in
/// [draft-hancke-tsvwg-snap-01](https://datatracker.ietf.org/doc/draft-hancke-tsvwg-snap/).
///
/// Each call generates fresh random values. The caller must hold onto the
/// returned bytes and pass them to [`ClientConfig::with_snap`] alongside
/// the remote peer's token.
pub fn generate_snap_token(config: &TransportConfig) -> Result<Bytes, crate::error::Error> {
    use crate::chunk::{Chunk, chunk_init::ChunkInit};
    use core::num::NonZeroU32;
    use rand::random;

    let mut init = ChunkInit {
        initiate_tag: random::<NonZeroU32>().get(),
        initial_tsn: random::<NonZeroU32>().get(),
        num_outbound_streams: u16::MAX,
        num_inbound_streams: u16::MAX,
        advertised_receiver_window_credit: config.max_receive_buffer_size(),
        ..Default::default()
    };
    init.set_supported_extensions();
    init.check()?;
    init.marshal()
}
