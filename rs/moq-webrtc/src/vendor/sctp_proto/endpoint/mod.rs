#[cfg(test)]
mod endpoint_test;

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use core::fmt;
use core::iter;
use core::net::{IpAddr, SocketAddr};
use core::ops::{Index, IndexMut};
use std::collections::HashMap;
use std::time::Instant;

use crate::chunk::{chunk_init::ChunkInit, chunk_type::CT_INIT};
use crate::config::MAX_SNAP_INIT_BYTES;
use crate::config::{ClientConfig, EndpointConfig, ServerConfig, TransportConfig};
use crate::packet::PartialDecode;
use crate::shared::AssociationEvent;
use crate::shared::{AssociationEventInner, AssociationId};
use crate::shared::{EndpointEvent, EndpointEventInner};
use crate::util::{AssociationIdGenerator, RandomAssociationIdGenerator};
use crate::{EcnCodepoint, Payload, Transmit};
use crate::{association::Association, chunk::Chunk};

use bytes::Bytes;
use log::{debug, trace, warn};
use rand::{SeedableRng, rngs::StdRng};
use rustc_hash::FxHashMap;
use slab::Slab;
use thiserror::Error;

/// The main entry point to the library
///
/// This object performs no I/O whatsoever. Instead, it generates a stream of packets to send via
/// `poll_transmit`, and consumes incoming packets and association-generated events via `handle` and
/// `handle_event`.
pub struct Endpoint {
    rng: StdRng,
    transmits: VecDeque<Transmit>,
    /// Identifies associations based on the INIT Dst AID the peer utilized
    ///
    /// Uses a standard `HashMap` to protect against hash collision attacks.
    association_ids_init: HashMap<AssociationId, AssociationHandle>,
    /// Identifies associations based on locally created CIDs
    ///
    /// Uses a cheaper hash function since keys are locally created
    association_ids: FxHashMap<AssociationId, AssociationHandle>,

    associations: Slab<AssociationMeta>,
    local_cid_generator: Box<dyn AssociationIdGenerator>,
    config: Arc<EndpointConfig>,
    server_config: Option<Arc<ServerConfig>>,
    /// Whether incoming associations should be unconditionally rejected by a server
    ///
    /// Equivalent to a `ServerConfig.accept_buffer` of `0`, but can
    /// be changed after the endpoint is constructed.
    reject_new_associations: bool,
}

impl fmt::Debug for Endpoint {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.debug_struct("Endpoint<T>")
            .field("rng", &self.rng)
            .field("transmits", &self.transmits)
            .field("association_ids_initial", &self.association_ids_init)
            .field("association_ids", &self.association_ids)
            .field("associations", &self.associations)
            .field("config", &self.config)
            .field("server_config", &self.server_config)
            .field("reject_new_associations", &self.reject_new_associations)
            .finish()
    }
}

impl Endpoint {
    /// Create a new endpoint
    ///
    /// Returns `Err` if the configuration is invalid.
    pub fn new(config: Arc<EndpointConfig>, server_config: Option<Arc<ServerConfig>>) -> Self {
        let rng = {
            let mut base = rand::rng();
            StdRng::from_rng(&mut base)
        };
        Self {
            rng,
            transmits: VecDeque::new(),
            association_ids_init: HashMap::default(),
            association_ids: FxHashMap::default(),
            associations: Slab::new(),
            local_cid_generator: (config.aid_generator_factory.as_ref())(),
            reject_new_associations: false,
            config,
            server_config,
        }
    }

    /// Get the next packet to transmit
    #[must_use]
    pub fn poll_transmit(&mut self) -> Option<Transmit> {
        self.transmits.pop_front()
    }

    /// Replace the server configuration, affecting new incoming associations only
    pub fn set_server_config(&mut self, server_config: Option<Arc<ServerConfig>>) {
        self.server_config = server_config;
    }

    /// Process `EndpointEvent`s emitted from related `Association`s
    ///
    /// In turn, processing this event may return a `AssociationEvent` for the same `Association`.
    pub fn handle_event(
        &mut self,
        ch: AssociationHandle,
        event: EndpointEvent,
    ) -> Option<AssociationEvent> {
        match event.0 {
            EndpointEventInner::Drained => {
                let conn = self.associations.remove(ch.0);
                self.association_ids_init.remove(&conn.init_cid);
                for cid in conn.loc_cids.values() {
                    self.association_ids.remove(cid);
                }
            }
        }
        None
    }

    /// Process an incoming UDP datagram
    pub fn handle(
        &mut self,
        now: Instant,
        remote: SocketAddr,
        local_ip: Option<IpAddr>,
        ecn: Option<EcnCodepoint>,
        data: Bytes,
    ) -> Option<(AssociationHandle, DatagramEvent)> {
        let partial_decode = match PartialDecode::unmarshal(&data) {
            Ok(x) => x,
            Err(err) => {
                trace!("malformed header: {}", err);
                return None;
            }
        };

        //
        // Handle packet on existing association, if any
        //
        let dst_cid = partial_decode.common_header.verification_tag;
        let known_ch = if dst_cid > 0 {
            self.association_ids.get(&dst_cid).cloned()
        } else {
            //TODO: improve INIT handling for DoS attack
            if partial_decode.first_chunk_type == CT_INIT {
                if let Some(dst_cid) = partial_decode.initiate_tag {
                    self.association_ids_init.get(&dst_cid).cloned()
                } else {
                    None
                }
            } else {
                None
            }
        };

        if let Some(ch) = known_ch {
            return Some((
                ch,
                DatagramEvent::AssociationEvent(AssociationEvent(AssociationEventInner::Datagram(
                    Transmit {
                        now,
                        remote,
                        ecn,
                        payload: Payload::PartialDecode(partial_decode),
                        local_ip,
                    },
                ))),
            ));
        }

        //
        // Potentially create a new association
        //
        self.handle_first_packet(now, remote, local_ip, ecn, partial_decode)
            .map(|(ch, a)| (ch, DatagramEvent::NewAssociation(a)))
    }

    /// Initiate an association.
    ///
    /// If SNAP tokens (INIT chunks) are provided via [`ClientConfig::with_snap`],
    /// the association will skip the SCTP 4-way handshake (RFC 4960 Section 5.1)
    /// and immediately transition to the ESTABLISHED state.
    ///
    /// Both `local_sctp_init` and `remote_sctp_init` must be provided for SNAP;
    /// use [`generate_snap_token`](crate::generate_snap_token) to create the
    /// local token.
    ///
    /// If only one side is set (e.g. the peer does not support SNAP), the
    /// association falls back to the normal SCTP handshake.
    ///
    /// **Note:** When using SNAP, **both** peers must call [`Endpoint::connect`]
    /// (there is no server-side SNAP via [`Endpoint::handle`]). Each peer
    /// generates its own token via [`generate_snap_token`](crate::generate_snap_token),
    /// exchanges it with the remote peer through a signaling channel (e.g.,
    /// SDP `a=sctp-init`), and then calls `connect` with `with_snap(local, remote)`.
    ///
    /// See [draft-hancke-tsvwg-snap-01](https://datatracker.ietf.org/doc/draft-hancke-tsvwg-snap/).
    pub fn connect(
        &mut self,
        config: ClientConfig,
        remote: SocketAddr,
    ) -> Result<(AssociationHandle, Association), ConnectError> {
        if self.is_full() {
            return Err(ConnectError::TooManyAssociations);
        }
        if remote.port() == 0 {
            return Err(ConnectError::InvalidRemoteAddress(remote));
        }
        if config.transport.local_port() == 0 || config.transport.remote_port() == 0 {
            return Err(ConnectError::InvalidSctpPort);
        }

        match (config.local_sctp_init, config.remote_sctp_init) {
            (Some(local_init), Some(remote_init)) => {
                self.connect_with_snap(config.transport, remote, local_init, remote_init)
            }
            (partial_local, partial_remote) => {
                if partial_local.is_some() || partial_remote.is_some() {
                    warn!(
                        "partial SNAP config: both local_sctp_init and \
                        remote_sctp_init must be set; falling back to normal handshake"
                    );
                }
                let remote_aid = RandomAssociationIdGenerator::new().generate_aid();
                let local_aid = self.new_aid();

                Ok(self.add_association(
                    remote_aid,
                    local_aid,
                    remote,
                    None,
                    Instant::now(),
                    None,
                    config.transport,
                ))
            }
        }
    }

    /// Create an association using SNAP (out-of-band exchanged INIT chunks).
    fn connect_with_snap(
        &mut self,
        transport: Arc<TransportConfig>,
        remote: SocketAddr,
        local_snap_bytes: Bytes,
        remote_snap_bytes: Bytes,
    ) -> Result<(AssociationHandle, Association), ConnectError> {
        // Enforce a size ceiling on raw INIT bytes before parsing.
        if local_snap_bytes.len() > MAX_SNAP_INIT_BYTES {
            return Err(ConnectError::Snap(SnapError::OversizedInit {
                side: SnapSide::Local,
                len: local_snap_bytes.len(),
            }));
        }
        if remote_snap_bytes.len() > MAX_SNAP_INIT_BYTES {
            return Err(ConnectError::Snap(SnapError::OversizedInit {
                side: SnapSide::Remote,
                len: remote_snap_bytes.len(),
            }));
        }

        let local_init = ChunkInit::unmarshal(&local_snap_bytes).map_err(|err| {
            ConnectError::Snap(SnapError::ParseFailed {
                side: SnapSide::Local,
                reason: err.to_string(),
            })
        })?;
        let remote_init = ChunkInit::unmarshal(&remote_snap_bytes).map_err(|err| {
            ConnectError::Snap(SnapError::ParseFailed {
                side: SnapSide::Remote,
                reason: err.to_string(),
            })
        })?;

        // Validate both chunks are INIT (not INIT-ACK)
        if local_init.is_ack {
            return Err(ConnectError::Snap(SnapError::InvalidInitAck {
                side: SnapSide::Local,
            }));
        }
        if remote_init.is_ack {
            return Err(ConnectError::Snap(SnapError::InvalidInitAck {
                side: SnapSide::Remote,
            }));
        }

        let local_aid = local_init.initiate_tag;
        let remote_aid = remote_init.initiate_tag;

        // RFC 4960 Section 3.3.2: The Initiate Tag MUST NOT take the value 0.
        if local_aid == 0 {
            return Err(ConnectError::Snap(SnapError::ZeroInitiateTag {
                side: SnapSide::Local,
            }));
        }
        if remote_aid == 0 {
            return Err(ConnectError::Snap(SnapError::ZeroInitiateTag {
                side: SnapSide::Remote,
            }));
        }

        // initial_tsn=0 is degenerate: wrapping_sub(1) yields u32::MAX for
        // peer_last_tsn, which would accept nearly all incoming TSNs.
        if local_init.initial_tsn == 0 {
            return Err(ConnectError::Snap(SnapError::ZeroInitialTsn {
                side: SnapSide::Local,
            }));
        }
        if remote_init.initial_tsn == 0 {
            return Err(ConnectError::Snap(SnapError::ZeroInitialTsn {
                side: SnapSide::Remote,
            }));
        }

        // RFC 4960 §3.3.2 / §5.1: validate stream counts, a_rwnd, etc.
        local_init.check().map_err(|err| {
            ConnectError::Snap(SnapError::InvalidInit {
                side: SnapSide::Local,
                reason: err.to_string(),
            })
        })?;
        remote_init.check().map_err(|err| {
            ConnectError::Snap(SnapError::InvalidInit {
                side: SnapSide::Remote,
                reason: err.to_string(),
            })
        })?;

        // Check for collision BEFORE allocating any resources.
        if local_aid == remote_aid {
            return Err(ConnectError::Snap(SnapError::AidCollision {
                kind: AidCollisionKind::LocalEqualsRemote,
                aid: local_aid,
            }));
        }
        if self.association_ids.contains_key(&local_aid) {
            return Err(ConnectError::Snap(SnapError::AidCollision {
                kind: AidCollisionKind::LocalInAssociationIds,
                aid: local_aid,
            }));
        }
        if self.association_ids.contains_key(&remote_aid) {
            return Err(ConnectError::Snap(SnapError::AidCollision {
                kind: AidCollisionKind::RemoteInAssociationIds,
                aid: remote_aid,
            }));
        }
        if self.association_ids_init.contains_key(&remote_aid) {
            return Err(ConnectError::Snap(SnapError::AidCollision {
                kind: AidCollisionKind::RemoteInAssociationIdsInit,
                aid: remote_aid,
            }));
        }
        if self.association_ids_init.contains_key(&local_aid) {
            return Err(ConnectError::Snap(SnapError::AidCollision {
                kind: AidCollisionKind::LocalInAssociationIdsInit,
                aid: local_aid,
            }));
        }

        let conn = Association::new_with_out_of_band_init(
            transport,
            self.config.get_max_payload_size(),
            remote,
            None,
            local_init,
            remote_init,
        )
        .map_err(|err| ConnectError::Snap(SnapError::AssociationFailed(err.to_string())))?;

        let id = self.associations.insert(AssociationMeta {
            init_cid: remote_aid,
            cids_issued: 1,
            loc_cids: iter::once((0, local_aid)).collect(),
            initial_remote: remote,
        });

        let ch = AssociationHandle(id);
        self.association_ids.insert(local_aid, ch);
        self.association_ids_init.insert(remote_aid, ch);

        debug!(
            "Created SNAP association: local_aid={:#x} remote_aid={:#x}",
            local_aid, remote_aid
        );

        Ok((ch, conn))
    }

    fn new_aid(&mut self) -> AssociationId {
        loop {
            let aid = self.local_cid_generator.generate_aid();
            if !self.association_ids.contains_key(&aid) {
                break aid;
            }
        }
    }

    fn handle_first_packet(
        &mut self,
        now: Instant,
        remote: SocketAddr,
        local_ip: Option<IpAddr>,
        ecn: Option<EcnCodepoint>,
        partial_decode: PartialDecode,
    ) -> Option<(AssociationHandle, Association)> {
        if partial_decode.first_chunk_type != CT_INIT
            || (partial_decode.first_chunk_type == CT_INIT && partial_decode.initiate_tag.is_none())
        {
            debug!("refusing first packet with Non-INIT or emtpy initial_tag INIT");
            return None;
        }

        let server_config = self.server_config.as_ref().unwrap();

        if self.associations.len() >= server_config.concurrent_associations as usize
            || self.reject_new_associations
            || self.is_full()
        {
            debug!("refusing association");
            //TODO: self.initial_close();
            return None;
        }

        let server_config = server_config.clone();
        let transport_config = server_config.transport.clone();

        let remote_aid = *partial_decode.initiate_tag.as_ref().unwrap();
        let local_aid = self.new_aid();

        let (ch, mut conn) = self.add_association(
            remote_aid,
            local_aid,
            remote,
            local_ip,
            now,
            Some(server_config),
            transport_config,
        );

        // Map the peer's INIT Initiate Tag so that retransmitted INITs (which use
        // verification_tag=0) can be routed to the association created for the first INIT.
        self.association_ids_init.insert(remote_aid, ch);

        conn.handle_event(AssociationEvent(AssociationEventInner::Datagram(
            Transmit {
                now,
                remote,
                ecn,
                payload: Payload::PartialDecode(partial_decode),
                local_ip,
            },
        )));

        Some((ch, conn))
    }

    #[allow(clippy::too_many_arguments)]
    fn add_association(
        &mut self,
        remote_aid: AssociationId,
        local_aid: AssociationId,
        remote_addr: SocketAddr,
        local_ip: Option<IpAddr>,
        now: Instant,
        server_config: Option<Arc<ServerConfig>>,
        transport_config: Arc<TransportConfig>,
    ) -> (AssociationHandle, Association) {
        let conn = Association::new(
            server_config,
            transport_config,
            self.config.get_max_payload_size(),
            local_aid,
            remote_addr,
            local_ip,
            now,
        );

        let id = self.associations.insert(AssociationMeta {
            init_cid: remote_aid,
            cids_issued: 1,
            loc_cids: iter::once((0, local_aid)).collect(),
            initial_remote: remote_addr,
        });

        let ch = AssociationHandle(id);
        self.association_ids.insert(local_aid, ch);

        (ch, conn)
    }

    /// Unconditionally reject future incoming associations
    pub fn reject_new_associations(&mut self) {
        self.reject_new_associations = true;
    }

    /// Access the configuration used by this endpoint
    pub fn config(&self) -> &EndpointConfig {
        &self.config
    }

    /// Whether we've used up 3/4 of the available AID space
    fn is_full(&self) -> bool {
        (((u32::MAX >> 1) + (u32::MAX >> 2)) as usize) < self.association_ids.len()
    }
}

#[derive(Debug)]
pub(crate) struct AssociationMeta {
    init_cid: AssociationId,
    /// Number of local association IDs.
    cids_issued: u64,
    loc_cids: FxHashMap<u64, AssociationId>,
    /// Remote address the association began with
    ///
    /// Only needed to support associations with zero-length AIDs, which cannot migrate, so we don't
    /// bother keeping it up to date.
    initial_remote: SocketAddr,
}

/// Internal identifier for an `Association` currently associated with an endpoint
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct AssociationHandle(pub usize);

impl From<AssociationHandle> for usize {
    fn from(x: AssociationHandle) -> usize {
        x.0
    }
}

impl Index<AssociationHandle> for Slab<AssociationMeta> {
    type Output = AssociationMeta;
    fn index(&self, ch: AssociationHandle) -> &AssociationMeta {
        &self[ch.0]
    }
}

impl IndexMut<AssociationHandle> for Slab<AssociationMeta> {
    fn index_mut(&mut self, ch: AssociationHandle) -> &mut AssociationMeta {
        &mut self[ch.0]
    }
}

/// Event resulting from processing a single datagram
#[allow(clippy::large_enum_variant)] // Not passed around extensively
pub enum DatagramEvent {
    /// The datagram is redirected to its `Association`
    AssociationEvent(AssociationEvent),
    /// The datagram has resulted in starting a new `Association`
    NewAssociation(Association),
}

/// Errors in the parameters being used to create a new association
///
/// These arise before any I/O has been performed.
#[non_exhaustive]
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ConnectError {
    /// The endpoint can no longer create new associations
    ///
    /// Indicates that a necessary component of the endpoint has been dropped or otherwise disabled.
    #[error("endpoint stopping")]
    EndpointStopping,
    /// The number of active associations on the local endpoint is at the limit
    ///
    /// Try using longer association IDs.
    #[error("too many associations")]
    TooManyAssociations,
    /// The domain name supplied was malformed
    #[error("invalid DNS name: {0}")]
    InvalidDnsName(String),
    /// The remote [`SocketAddr`] supplied was malformed
    ///
    /// Examples include attempting to connect to port 0, or using an inappropriate address family.
    #[error("invalid remote address: {0}")]
    InvalidRemoteAddress(SocketAddr),
    /// The configured SCTP source or destination port is zero.
    #[error("SCTP source and destination ports must be nonzero")]
    InvalidSctpPort,
    /// No default client configuration was set up
    ///
    /// Use `Endpoint::connect_with` to specify a client configuration.
    #[error("no default client config")]
    NoDefaultClientConfig,
    /// Out-of-band SNAP setup error.
    #[error("SNAP error: {0}")]
    Snap(#[from] SnapError),
}

/// Which peer's token (INIT chunk) caused a [`SnapError`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapSide {
    /// The locally-generated token.
    Local,
    /// The remote peer's token.
    Remote,
}

impl fmt::Display for SnapSide {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SnapSide::Local => f.write_str("local"),
            SnapSide::Remote => f.write_str("remote"),
        }
    }
}

/// Which routing table detected an association-ID collision in [`SnapError::AidCollision`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AidCollisionKind {
    /// The local and remote association IDs are equal.
    LocalEqualsRemote,
    /// The local AID is already present in `association_ids`.
    LocalInAssociationIds,
    /// The remote AID is already present in `association_ids`.
    RemoteInAssociationIds,
    /// The remote AID is already present in `association_ids_init`.
    RemoteInAssociationIdsInit,
    /// The local AID is already present in `association_ids_init`.
    LocalInAssociationIdsInit,
}

impl fmt::Display for AidCollisionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AidCollisionKind::LocalEqualsRemote => f.write_str("local_aid equals remote_aid"),
            AidCollisionKind::LocalInAssociationIds => f.write_str("local_aid in association_ids"),
            AidCollisionKind::RemoteInAssociationIds => {
                f.write_str("remote_aid in association_ids")
            }
            AidCollisionKind::RemoteInAssociationIdsInit => {
                f.write_str("remote_aid in association_ids_init")
            }
            AidCollisionKind::LocalInAssociationIdsInit => {
                f.write_str("local_aid in association_ids_init")
            }
        }
    }
}

/// Specific errors that can occur during SNAP (out-of-band token) setup.
///
/// These are not part of the stable API — match on [`ConnectError::Snap`]
/// and use the `Display` impl for user-facing messages.
#[non_exhaustive]
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SnapError {
    /// Failed to parse token (INIT chunk) bytes.
    #[error("failed to parse {side} SNAP token: {reason}")]
    ParseFailed {
        /// Which side's token failed to parse.
        side: SnapSide,
        /// The underlying parse error message.
        reason: String,
    },
    /// The provided bytes were an INIT-ACK, not an INIT.
    #[error("invalid {side} SNAP token: expected INIT, got INIT-ACK")]
    InvalidInitAck {
        /// Which side provided an INIT-ACK instead of an INIT.
        side: SnapSide,
    },
    /// The token's `initiate_tag` is zero (RFC 4960 §3.3.2 violation).
    #[error("{side} SNAP token has zero initiate_tag")]
    ZeroInitiateTag {
        /// Which side has the zero tag.
        side: SnapSide,
    },
    /// The token's `initial_tsn` is zero.
    #[error("{side} SNAP token has zero initial_tsn")]
    ZeroInitialTsn {
        /// Which side has the zero TSN.
        side: SnapSide,
    },
    /// An association ID collision was detected.
    #[error("SNAP collision: {kind} {aid:#x} already in use")]
    AidCollision {
        /// Which routing table collided.
        kind: AidCollisionKind,
        /// The colliding association ID.
        aid: u32,
    },
    /// The token (INIT chunk) bytes exceed the maximum allowed size.
    #[error("{side} SNAP token too large: {len} bytes (max {max})", max = MAX_SNAP_INIT_BYTES)]
    OversizedInit {
        /// Which side sent oversized bytes.
        side: SnapSide,
        /// Actual byte length.
        len: usize,
    },
    /// The parsed token (INIT chunk) failed RFC 4960 validation.
    #[error("invalid {side} SNAP token: {reason}")]
    InvalidInit {
        /// Which side has the invalid token.
        side: SnapSide,
        /// The underlying validation error message.
        reason: String,
    },
    /// Association creation failed.
    #[error("failed to create SNAP association: {0}")]
    AssociationFailed(String),
}
