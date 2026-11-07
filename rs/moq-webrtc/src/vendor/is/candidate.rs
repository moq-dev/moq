use crate::IceError;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize, Serializer};
use std::collections::hash_map::DefaultHasher;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::net::{IpAddr, SocketAddr};
use str0m_proto::{Protocol, TcpType};

/// ICE candidates are network addresses used to connect to a peer.
///
/// There are different kinds of ICE candidates. The simplest kind is a
/// host candidate which is a socket address on a local (host) network interface.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Candidate {
    /// An arbitrary string used in the freezing algorithm to
    /// group similar candidates.
    ///
    /// It is the same for two candidates that
    /// have the same type, base IP address, protocol (UDP, TCP, etc.),
    /// and STUN or TURN server.  If any of these are different, then the
    /// foundation will be different.
    ///
    /// For remote, this is communicated,  and locally it's calculated.
    foundation: Option<String>, // 1-32 "ice chars", ALPHA / DIGIT / "+" / "/"

    /// A component is a piece of a data stream.
    ///
    /// A data stream may require multiple components, each of which has to
    /// work in order for the data stream as a whole to work.  For RTP/RTCP
    /// data streams, unless RTP and RTCP are multiplexed in the same port,
    /// there are two components per data stream -- one for RTP, and one
    /// for RTCP.
    component_id: u16, // 1 for RTP, 2 for RTCP

    /// Protocol for the candidate.
    proto: Protocol,

    /// Priority.
    ///
    /// For remote, this is communicated, and locally it's (mostly) calculated.
    /// For local peer reflexive it is set.
    prio: Option<u32>, // 1-10 digits

    /// The actual address to use. This might be a host address, server reflex, relay etc.
    addr: SocketAddr, // ip/port

    /// The base address
    ///
    /// "Base" refers to the address an agent sends from for a
    /// particular candidate.  Thus, as a degenerate case, host candidates
    /// also have a base, but it's the same as the host candidate.
    ///
    /// * host - same as `addr`, i.e the local interface address
    /// * peer/server reflexive - the local interface address
    /// * relay - same as `addr`, the allocation on the TURN server
    base: Option<SocketAddr>, // the "base" used for local candidates.

    /// Type of candidate.
    kind: CandidateKind, // host/srflx/prflx/relay

    /// TCP connection role specified by the optional `tcptype` ICE candidate
    /// extension.
    tcptype: Option<TcpType>,

    /// Related address.
    ///
    /// For server-reflexive candidates, this is the internal IP/port the candidate corresponds to
    /// (the one behind the NAT, usually). For relay candidates, this is the mapped address selected
    /// by the TURN server.
    raddr: Option<SocketAddr>, // ip/port

    /// Ufrag.
    ///
    /// This is used to tie an ice candidate to a specific ICE session. It's important
    /// when trickle ICE is used in conjunction with ice restart, since it must be
    /// possible the ice agent to know whether a candidate appearing belongs to
    /// the current or previous session.
    ///
    /// This value is only set for incoming candidates. Once we use the candidate inside
    /// pairs, the field is blanked to not be confusing during ice-restarts.
    ufrag: Option<String>,

    /// The address of the local interface.
    ///
    /// * host - same as `addr`
    /// * peer/server reflexive - same as `base`
    /// * relay - set explicitly
    ///
    /// This is an extension to the ICE spec that we use to track
    /// local interfaces also for relayed candidates.
    local: SocketAddr,

    /// The ice agent might assign a local preference if we have multiple candidates
    /// that are the same type.
    local_preference: Option<u32>,

    /// If we discarded this candidate (for example due to being redundant
    /// against another candidate).
    discarded: bool,
}

impl fmt::Debug for Candidate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Candidate({}={}/{}", self.kind, self.addr, self.proto)?;
        if let Some(base) = self.base {
            if base != self.addr {
                write!(f, " base={base}")?;
            }
        }
        if let Some(raddr) = self.raddr {
            write!(f, " raddr={raddr}")?;
        }
        write!(f, " prio={}", self.prio())?;
        if self.discarded {
            write!(f, " discarded")?;
        }
        write!(f, ")")
    }
}

impl Candidate {
    /// Starts the typesafe builder.
    ///
    /// # Example
    ///
    /// ```
    /// # use is::Candidate;
    /// # use is::IceError;
    /// # use str0m_proto::TcpType;
    /// # use std::net::SocketAddr;
    ///
    /// let addr: SocketAddr = "192.168.1.1:12345".parse().unwrap();
    ///
    /// // A standard UDP host candidate
    /// let udp_host = Candidate::builder()
    ///     .udp()
    ///     .host(addr)
    ///     .build()?;
    ///
    /// // A TCP host candidate with a passive role
    /// let tcp_host = Candidate::builder()
    ///     .tcp()
    ///     .host(addr)
    ///     .tcptype(TcpType::Passive)
    ///     .build()?;
    ///
    /// # Ok::<(), IceError>(())
    /// ```
    pub fn builder() -> CandidateBuilder<NoProtocol, NoRoute> {
        CandidateBuilder {
            protocol_state: NoProtocol(()),
            route_state: NoRoute(()),
            foundation: None,
            component_id: 1, // Default RTP
            prio: None,
            tcptype: None,
            raddr: None,
            ufrag: None,
            local_preference: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn new(
        foundation: Option<String>,
        component_id: u16,
        proto: Protocol,
        prio: Option<u32>,
        addr: SocketAddr,
        base: Option<SocketAddr>,
        kind: CandidateKind,
        raddr: Option<SocketAddr>,
        tcptype: Option<TcpType>,
        ufrag: Option<String>,
        local: SocketAddr,
    ) -> Self {
        Candidate {
            foundation,
            component_id,
            proto,
            prio,
            addr,
            base,
            kind,
            raddr,
            tcptype,
            ufrag,
            local,
            local_preference: None,
            discarded: false,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn from_parts(
        foundation: String,
        component_id: u16,
        proto: Protocol,
        prio: u32,
        addr: SocketAddr,
        kind: CandidateKind,
        raddr: Option<SocketAddr>,
        tcptype: Option<TcpType>,
        ufrag: Option<String>,
    ) -> Self {
        Candidate::new(
            Some(foundation),
            component_id,
            proto,
            Some(prio),
            addr,
            None,
            kind,
            raddr,
            tcptype,
            ufrag,
            match kind {
                CandidateKind::Host => addr,
                CandidateKind::PeerReflexive
                | CandidateKind::ServerReflexive
                | CandidateKind::Relayed => Self::arbitrary_raddr(addr),
            },
        )
    }

    /// Creates a host ICE candidate.
    ///
    /// Host candidates are local sockets directly on the host.
    pub fn host(addr: SocketAddr, proto: impl TryInto<Protocol>) -> Result<Self, IceError> {
        if !is_valid_ip(addr.ip()) {
            return Err(IceError::BadCandidate(format!("invalid ip {}", addr.ip())));
        }

        Ok(Candidate::new(
            None,
            1, // only RTP
            parse_proto(proto)?,
            None,
            addr,
            Some(addr),
            CandidateKind::Host,
            None,
            None,
            None,
            addr,
        ))
    }

    /// Creates a server reflexive ICE candidate.
    ///
    /// Server reflexive candidates are local sockets mapped to external ip discovered
    /// via a STUN binding request.
    /// The `base` is the local interface that this address corresponds to.
    pub fn server_reflexive(
        addr: SocketAddr,
        base: SocketAddr,
        proto: impl TryInto<Protocol>,
    ) -> Result<Self, IceError> {
        if !is_valid_ip(addr.ip()) {
            return Err(IceError::BadCandidate(format!("invalid ip {}", addr.ip())));
        }

        if addr.is_ipv4() != base.is_ipv4() {
            return Err(IceError::BadCandidate(
                "addr and base are different IP versions".to_owned(),
            ));
        }

        Ok(Candidate::new(
            None,
            1, // only RTP
            parse_proto(proto)?,
            None,
            addr,
            Some(base),
            CandidateKind::ServerReflexive,
            Some(Self::arbitrary_raddr(addr)),
            None,
            None,
            base,
        ))
    }

    /// Creates a relayed ICE candidate.
    ///
    /// Relayed candidates are server sockets relaying traffic to a local socket.
    /// Allocate a TURN addr to use as a local candidate.
    ///
    /// * `addr` - The TURN server's allocated address for relaying traffic.
    ///   This is the address used for communication with the peer.
    /// * `local` - The local interface address for this candidate.
    ///   This is the address from which the TURN allocation request was sent.
    /// * `proto` - The transport protocol to use (UDP, TCP, etc.).
    pub fn relayed(
        addr: SocketAddr,
        local: SocketAddr,
        proto: impl TryInto<Protocol>,
    ) -> Result<Self, IceError> {
        if !is_valid_ip(addr.ip()) {
            return Err(IceError::BadCandidate(format!("invalid ip {}", addr.ip())));
        }

        Ok(Candidate::new(
            None,
            1, // only RTP
            parse_proto(proto)?,
            None,
            addr,
            Some(addr),
            CandidateKind::Relayed,
            Some(Self::arbitrary_raddr(addr)),
            None,
            None,
            local,
        ))
    }

    /// Creates a new ICE candidate from a candidate attribute string.
    ///
    /// Expects the format from RFC 5245 section 15.1, e.g.
    /// `candidate:1 1 udp 2113929471 203.0.113.100 10100 typ host`.
    pub fn from_sdp_string(s: &str) -> Result<Self, IceError> {
        crate::parse::parse_candidate(s)
    }

    /// Creates a peer reflexive ICE candidate.
    ///
    /// Peer reflexive candidates are NAT:ed addresses discovered via STUN
    /// binding responses. `addr` is the discovered address. `base` is the local
    /// (host) address inside the NAT we used to get this response.
    pub(crate) fn peer_reflexive(
        proto: impl TryInto<Protocol>,
        addr: SocketAddr,
        base: SocketAddr,
        prio: u32,
        found: Option<String>,
        ufrag: String,
    ) -> Self {
        Candidate::new(
            found,
            1, // only RTP
            parse_proto(proto).expect("internal call to have correct protocol"),
            Some(prio),
            addr,
            Some(base),
            CandidateKind::PeerReflexive,
            None,
            None,
            Some(ufrag),
            base,
        )
    }

    /// Create an arbitrary socket address, matching the format of the input address,
    /// for placement in the `raddr` field.
    ///
    /// For non-host candidates, Firefox (and perhaps others) require the SDP string to
    /// contain `raddr` and `rport` to correctly parse. While we could put honest values
    /// here, those honest values are likely private IP addresses that we would rather not
    /// expose to the world. Instead, browsers often spoof values to go here instead, and
    /// we do the same.
    ///
    /// Note that `raddr` and `rport` are only for diagnostic purposes, and have no
    /// bearing on ICE connectivity checks.
    fn arbitrary_raddr(s: SocketAddr) -> SocketAddr {
        use std::net::{Ipv4Addr, Ipv6Addr, SocketAddrV4, SocketAddrV6};
        match s {
            SocketAddr::V4(_) => SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)),
            SocketAddr::V6(_) => SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 0, 0, 0)),
        }
    }

    #[cfg(test)]
    pub(crate) fn test_peer_rflx(
        addr: SocketAddr,
        base: SocketAddr,
        proto: impl TryInto<Protocol>,
    ) -> Self {
        Candidate::new(
            None,
            1, // only RTP
            parse_proto(proto).expect("internal test to have correct protocol"),
            None,
            addr,
            Some(base),
            CandidateKind::PeerReflexive,
            None,
            None,
            None,
            base,
        )
    }

    /// Candidate foundation.
    ///
    /// For local candidates this is calculated.
    pub(crate) fn foundation(&self) -> String {
        if let Some(v) = &self.foundation {
            return v.clone();
        }

        // Two candidates have the same foundation when all of the
        // following are true:
        let mut hasher = DefaultHasher::new();

        //  o  They have the same type (host, relayed, server reflexive, or peer
        //     reflexive).
        self.kind.hash(&mut hasher);

        //  o  Their bases have the same IP address (the ports can be different).
        self.base().ip().hash(&mut hasher);

        //  o  For reflexive and relayed candidates, the STUN or TURN servers
        //     used to obtain them have the same IP address (the IP address used
        //     by the agent to contact the STUN or TURN server).
        if let Some(raddr) = self.raddr {
            raddr.ip().hash(&mut hasher);
        }

        //  o  They were obtained using the same transport protocol (TCP, UDP).
        self.proto.hash(&mut hasher);

        let hash = hasher.finish();

        format!("{:08x}{hash:x}", self.prio().to_be())
    }

    /// Returns the priority value for the specified ICE candidate.
    ///
    /// The priority is a positive integer between 1 and 2^31 - 1 (inclusive), calculated
    /// according to the ICE specification defined in RFC 8445, Section 5.1.2.
    pub fn prio(&self) -> u32 {
        self.do_prio(false)
    }

    pub(crate) fn prio_prflx(&self) -> u32 {
        self.do_prio(true)
    }

    fn do_prio(&self, as_prflx: bool) -> u32 {
        // Remote candidates have their prio calculated on their side.
        if let Some(prio) = &self.prio {
            return *prio;
        }

        let kind = if as_prflx {
            CandidateKind::PeerReflexive
        } else {
            self.kind
        };

        // Per RFC5245 Sec. 4.1.2.1, the RECOMMENDED values for type preferences are
        // 126 for host candidates, 110 for peer-reflexive candidates, 100 for
        // server-reflexive candidates, and 0 for relayed candidates. The variations
        // for non-UDP protocols are taken from libwebrtc:
        // <https://webrtc.googlesource.com/src/+/refs/heads/main/p2p/base/port.h#68>
        let type_preference = match (kind, self.proto) {
            (CandidateKind::Host, Protocol::Udp) => 126,
            (CandidateKind::PeerReflexive, Protocol::Udp) => 110,
            (CandidateKind::ServerReflexive, _) => 100,
            (CandidateKind::Host, _) => 90,
            (CandidateKind::PeerReflexive, _) => 80,
            (CandidateKind::Relayed, Protocol::Udp) => 2,
            (CandidateKind::Relayed, Protocol::Tcp) => 1,
            (CandidateKind::Relayed, _) => 0,
        };

        // The recommended formula combines a preference for the candidate type
        // (server reflexive, peer reflexive, relayed, and host), a preference
        // for the IP address for which the candidate was obtained, and a
        // component ID using the following formula:
        //
        // priority = (2^24)*(type preference) +
        //     (2^8)*(local preference) +
        //     (2^0)*(256 - component ID)
        let prio =
            type_preference << 24 | self.local_preference() << 8 | (256 - self.component_id as u32);

        // https://datatracker.ietf.org/doc/html/rfc8445#section-5.1.2
        // MUST be a positive integer between 1 and (2**31 - 1)
        assert!(prio >= 1 && prio < 2_u32.pow(31));

        prio
    }

    pub(crate) fn local_preference(&self) -> u32 {
        self.local_preference
            .unwrap_or_else(|| if self.addr.is_ipv6() { 65_535 } else { 65_534 })
    }

    pub(crate) fn component_id(&self) -> u16 {
        self.component_id
    }

    /// Returns the address for the specified ICE candidate.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Returns the _local_ address of this ICE candidate.
    ///
    /// This is an extension to the ICE spec that we use to track
    /// local interfaces also for relayed candidates.
    /// Specifically, for relayed candidates, the local interface
    /// is the address the agent uses to communicate with the TURN
    /// server using the TURN protocol.
    ///
    /// For all other candidates, this refers to either the
    /// candidate itself (host) or the base (srflx/prflx).
    pub fn local(&self) -> SocketAddr {
        self.local
    }

    /// Returns a reference to the String containing the transport protocol of
    /// the ICE candidate. For example tcp/udp/..
    pub fn proto(&self) -> Protocol {
        self.proto
    }

    pub(crate) fn base(&self) -> SocketAddr {
        self.base.unwrap_or(self.addr)
    }

    pub(crate) fn raddr(&self) -> Option<SocketAddr> {
        self.raddr
    }

    /// Returns the candidate's TCP role. Refer to [`TcpType`] for more details.
    pub fn tcptype(&self) -> Option<TcpType> {
        self.tcptype
    }

    /// Returns the kind of this candidate.
    pub fn kind(&self) -> CandidateKind {
        self.kind
    }

    pub(crate) fn set_local_preference(&mut self, v: u32) {
        self.local_preference = Some(v);
    }

    pub(crate) fn set_discarded(&mut self, discarded: bool) {
        self.discarded = discarded;
    }

    pub(crate) fn discarded(&self) -> bool {
        self.discarded
    }

    pub fn set_ufrag(&mut self, ufrag: &str) {
        self.ufrag = Some(ufrag.into());
    }

    pub fn ufrag(&self) -> Option<&str> {
        self.ufrag.as_deref()
    }

    pub(crate) fn clear_ufrag(&mut self) {
        self.ufrag = None;
    }

    /// Generates a candidate attribute string.
    pub fn to_sdp_string(&self) -> String {
        let mut s = format!(
            "candidate:{} {} {} {} {} {} typ {}",
            self.foundation(),
            self.component_id,
            self.proto,
            self.prio(),
            self.addr.ip(),
            self.addr.port(),
            self.kind
        );
        if let Some(raddr) = &self.raddr {
            s.push_str(&format!(" raddr {} rport {}", raddr.ip(), raddr.port()))
        }
        // https://datatracker.ietf.org/doc/html/rfc6544#section-4.5
        // the RFC requires tcptype to be defined strictly after raddr if exists.
        if let Some(tcptype) = &self.tcptype {
            s.push_str(&format!(" tcptype {}", tcptype));
        }
        if let Some(ufrag) = &self.ufrag {
            s.push_str(&format!(" ufrag {}", ufrag));
        }
        s
    }
}

impl fmt::Display for Candidate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_sdp_string())
    }
}

fn parse_proto(proto: impl TryInto<Protocol>) -> Result<Protocol, IceError> {
    proto
        .try_into()
        .map_err(|_| IceError::BadCandidate("invalid protocol".into()))
}

/// Type of candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CandidateKind {
    /// Host (local network interface)
    Host,
    /// Prflx (Peer reflexive)
    PeerReflexive,
    /// Srflx (STUN)
    ServerReflexive,
    /// Relay (TURN)
    Relayed,
}

impl fmt::Display for CandidateKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let x = match self {
            CandidateKind::Host => "host",
            CandidateKind::PeerReflexive => "prflx",
            CandidateKind::ServerReflexive => "srflx",
            CandidateKind::Relayed => "relay",
        };
        write!(f, "{x}")
    }
}

fn is_valid_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            !v.is_link_local() && !v.is_broadcast() && !v.is_multicast() && !v.is_unspecified()
        }
        IpAddr::V6(v) => !v.is_multicast() && !v.is_unspecified(),
    }
}

/// Serialize [Candidate] into candidate info.
///
/// Always set `sdpMid` to null and `sdpMLineIndex` to 0, as we only support one media line.
///
/// e.g. serde_json would produce:
/// ```json
/// {
///  "candidate": "candidate:12044049749558888150 1 udp 2130706175 1.2.3.4 1234 typ host",
///  "sdpMid": null,
///  "sdpMLineIndex": 0
///  "usernameFragment": "ufrag"
/// }
/// ```
impl Serialize for Candidate {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut o = serializer.serialize_struct("CandidateInfo", 4)?;
        o.serialize_field("candidate", &self.to_sdp_string())?;
        o.serialize_field("sdpMid", &None::<()>)?;
        o.serialize_field("sdpMLineIndex", &0)?;
        o.serialize_field("usernameFragment", &self.ufrag())?;
        o.end()
    }
}

/// Deserialize [Candidate] from a candidate info.
///
/// Similar to [Candidate::serialize], we drop `sdpMid` and `sdpMLineIndex` when parsing.
impl<'de> Deserialize<'de> for Candidate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct CandidateInfo {
            candidate: String,
            username_fragment: Option<String>,
        }

        let CandidateInfo {
            candidate,
            username_fragment,
        } = CandidateInfo::deserialize(deserializer)?;

        let mut candidate =
            Candidate::from_sdp_string(&candidate).map_err(serde::de::Error::custom)?;

        if let Some(ufrag) = username_fragment {
            candidate.set_ufrag(&ufrag);
        }

        Ok(candidate)
    }
}

#[doc(hidden)]
/// Marker for a builder requiring a protocol selection.
pub struct NoProtocol(());

#[doc(hidden)]
/// State indicating a transport protocol has been assigned to the builder.
pub struct HasProtocol<P> {
    _marker: PhantomData<P>,
    protocol: Protocol,
}

#[doc(hidden)]
/// Marker for a builder using the UDP protocol.
pub struct Udp(());

#[doc(hidden)]
/// Marker for a builder using a TCP-based protocol.
pub struct Tcp(());

#[doc(hidden)]
/// Marker indicating the candidate's network route (addresses and kind) is not yet defined.
pub struct NoRoute(());

#[doc(hidden)]
/// State containing the network route and addresses required to build a candidate.
pub struct HasRoute {
    kind: CandidateKind,
    addr: SocketAddr,
    base: SocketAddr,
    local: SocketAddr,
}

/// A typesafe builder for constructing a [`Candidate`].
///
/// This builder uses the Type State Pattern to enforce correct construction order and
/// protocol-specific constraints (such as preventing `tcptype` on UDP).
pub struct CandidateBuilder<P, R> {
    foundation: Option<String>,
    component_id: u16,
    prio: Option<u32>,
    tcptype: Option<TcpType>,
    raddr: Option<SocketAddr>,
    ufrag: Option<String>,
    local_preference: Option<u32>,

    protocol_state: P,
    route_state: R,
}

// Step 1: Protocol Selection
impl CandidateBuilder<NoProtocol, NoRoute> {
    /// Sets the protocol to UDP.
    pub fn udp(self) -> CandidateBuilder<HasProtocol<Udp>, NoRoute> {
        self.into_has_protocol(Protocol::Udp)
    }

    /// Sets the protocol to standard TCP.
    pub fn tcp(self) -> CandidateBuilder<HasProtocol<Tcp>, NoRoute> {
        self.into_has_protocol(Protocol::Tcp)
    }

    /// Sets the protocol to SSL-over-TCP.
    pub fn ssl_tcp(self) -> CandidateBuilder<HasProtocol<Tcp>, NoRoute> {
        self.into_has_protocol(Protocol::SslTcp)
    }

    /// Sets the protocol to TLS.
    pub fn tls(self) -> CandidateBuilder<HasProtocol<Tcp>, NoRoute> {
        self.into_has_protocol(Protocol::Tls)
    }

    fn into_has_protocol<NewP>(self, p: Protocol) -> CandidateBuilder<HasProtocol<NewP>, NoRoute> {
        CandidateBuilder {
            protocol_state: HasProtocol {
                _marker: PhantomData,
                protocol: p,
            },
            route_state: self.route_state,
            foundation: self.foundation,
            component_id: self.component_id,
            prio: self.prio,
            tcptype: self.tcptype,
            raddr: self.raddr,
            ufrag: self.ufrag,
            local_preference: self.local_preference,
        }
    }
}

// Step 2: Route Selection
impl<P> CandidateBuilder<HasProtocol<P>, NoRoute> {
    /// Configures as a host candidate.
    pub fn host(self, addr: SocketAddr) -> CandidateBuilder<HasProtocol<P>, HasRoute> {
        self.into_has_route(CandidateKind::Host, addr, addr, addr, None)
    }

    /// Configures as a Server Reflexive (STUN) candidate.
    /// `base` and `addr` must use the same IP version.
    pub fn server_reflexive(
        self,
        addr: SocketAddr,
        base: SocketAddr,
    ) -> CandidateBuilder<HasProtocol<P>, HasRoute> {
        self.into_has_route(
            CandidateKind::ServerReflexive,
            addr,
            base,
            base,
            Some(Candidate::arbitrary_raddr(addr)),
        )
    }

    /// Configures as a Relayed (TURN) candidate.
    /// Base is set to `addr`, and `local` is the interface address.
    pub fn relayed(
        self,
        addr: SocketAddr,
        local: SocketAddr,
    ) -> CandidateBuilder<HasProtocol<P>, HasRoute> {
        self.into_has_route(
            CandidateKind::Relayed,
            addr,
            addr,
            local,
            Some(Candidate::arbitrary_raddr(addr)),
        )
    }

    fn into_has_route(
        self,
        kind: CandidateKind,
        addr: SocketAddr,
        base: SocketAddr,
        local: SocketAddr,
        raddr: Option<SocketAddr>,
    ) -> CandidateBuilder<HasProtocol<P>, HasRoute> {
        CandidateBuilder {
            protocol_state: self.protocol_state,
            route_state: HasRoute {
                kind,
                addr,
                base,
                local,
            },
            raddr,
            foundation: self.foundation,
            component_id: self.component_id,
            prio: self.prio,
            tcptype: self.tcptype,
            ufrag: self.ufrag,
            local_preference: self.local_preference,
        }
    }
}

// Step 3: Final General Configurations and Build
impl<P> CandidateBuilder<HasProtocol<P>, HasRoute> {
    /// Consumes the builder and returns a [`Candidate`].
    pub fn build(self) -> Result<Candidate, IceError> {
        let route = &self.route_state;
        if !is_valid_ip(route.addr.ip()) {
            return Err(IceError::BadCandidate(format!(
                "Invalid IP: {}",
                route.addr.ip()
            )));
        }

        if route.addr.is_ipv4() != route.base.is_ipv4() {
            return Err(IceError::BadCandidate(format!(
                "address IP version mismatch: addr={} base={}",
                route.addr.ip(),
                route.base.ip(),
            )));
        }

        Ok(Candidate::new(
            self.foundation,
            self.component_id,
            self.protocol_state.protocol,
            self.prio,
            route.addr,
            Some(route.base),
            route.kind,
            self.raddr,
            self.tcptype,
            self.ufrag,
            route.local,
        ))
    }
}

impl<R> CandidateBuilder<HasProtocol<Tcp>, R> {
    /// Configures the TCP type (active, passive, so).
    pub fn tcptype(mut self, t: TcpType) -> Self {
        self.tcptype = Some(t);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_serialize_deserialize() {
        let socket_addr = "1.2.3.4:9876".parse().unwrap();
        let candidates = [
            Candidate::host(socket_addr, Protocol::Udp).unwrap(),
            Candidate::host(socket_addr, Protocol::Tcp).unwrap(),
            Candidate::builder()
                .tcp()
                .host(socket_addr)
                .tcptype(TcpType::Passive)
                .build()
                .unwrap(),
        ];

        for c1 in &candidates {
            let json = serde_json::to_string(&c1).unwrap();
            let c2: Candidate = serde_json::from_str(&json).unwrap();
            assert_eq!(c1.proto, c2.proto);
            assert_eq!(c1.tcptype, c2.tcptype);

            // Can't test equality because foundation is calculated on the fly. Use string compare instead.
            assert_eq!(c1.to_string(), c2.to_string());
        }
    }

    #[test]
    fn serialize() {
        let socket_addr = "1.2.3.4:9876".parse().unwrap();
        let mut candidate = Candidate::host(socket_addr, Protocol::Udp).unwrap();
        let json = no_hash(serde_json::to_string(&candidate).unwrap());
        let got: serde_json::Value = serde_json::from_str(&json).unwrap();
        let expected = serde_json::json!({
            "candidate": "candidate:--- 1 udp 2130706175 1.2.3.4 9876 typ host",
            "sdpMid": null,
            "sdpMLineIndex": 0,
            "usernameFragment": null
        });
        assert_eq!(got, expected);

        // Add a username fragment
        candidate.ufrag = Some("ufrag".to_string());
        let json = no_hash(serde_json::to_string(&candidate).unwrap());
        let got: serde_json::Value = serde_json::from_str(&json).unwrap();
        let expected = serde_json::json!({
            "candidate": "candidate:--- 1 udp 2130706175 1.2.3.4 9876 typ host ufrag ufrag",
            "sdpMid": null,
            "sdpMLineIndex": 0,
            "usernameFragment": "ufrag"
        });
        assert_eq!(got, expected);
    }

    fn no_hash(mut s: String) -> String {
        let f = s.find("candidate:").unwrap();
        let t = s.find(" 1 ").unwrap();
        s.replace_range((f + 10)..t, "---");
        s
    }

    #[test]
    fn deserialize() {
        let json = serde_json::json!({
            "candidate": "candidate:12044049749558888150 1 udp 2130706175 1.2.3.4 9876 typ host ufrag ufrag",
            "sdpMid": "ignored",
            "sdpMLineIndex": 123,
            "usernameFragment": "ufrag"
        }).to_string();
        let candidate: Candidate = serde_json::from_str(&json).unwrap();
        assert_eq!(candidate.ufrag(), Some("ufrag"));
        assert_eq!(candidate.addr().to_string(), "1.2.3.4:9876");
        assert_eq!(candidate.base().to_string(), "1.2.3.4:9876");
        assert_eq!(candidate.kind(), CandidateKind::Host);
        assert_eq!(candidate.proto(), Protocol::Udp);
        assert_eq!(candidate.prio(), 2130706175);
        assert_eq!(candidate.component_id(), 1);
        assert_eq!(candidate.raddr(), None);
        assert!(!candidate.discarded());
    }

    #[test]
    fn to_string() {
        let local_addr = "7.8.9.0:2345".parse().unwrap();
        let socket_addr = "1.2.3.4:9876".parse().unwrap();
        let mut candidate = Candidate::host(socket_addr, Protocol::Udp).unwrap();
        assert_eq!(
            no_hash(candidate.to_string()),
            "candidate:--- 1 udp 2130706175 1.2.3.4 9876 typ host"
        );

        candidate.ufrag = Some("ufrag".into());
        assert_eq!(
            no_hash(candidate.to_string()),
            "candidate:--- 1 udp 2130706175 1.2.3.4 9876 typ host ufrag ufrag"
        );

        candidate.raddr = Some("5.5.5.5:5555".parse().unwrap());
        assert_eq!(
            no_hash(candidate.to_string()),
            "candidate:--- 1 udp 2130706175 1.2.3.4 9876 typ host raddr 5.5.5.5 rport 5555 ufrag ufrag"
        );

        // let base_addr = "5.6.7.8:4321".parse().unwrap();

        let candidate = Candidate::relayed(socket_addr, local_addr, Protocol::SslTcp).unwrap();
        assert_eq!(
            no_hash(candidate.to_string()),
            "candidate:--- 1 ssltcp 16776959 1.2.3.4 9876 typ relay raddr 0.0.0.0 rport 0"
        );
    }

    #[test]
    fn new_from_sdp_string() {
        let candidate = Candidate::from_sdp_string(
            "candidate:fffeff7e5e895846293d220a 1 udp 2130706175 1.2.3.4 9876 typ host ufrag myuserfrag",
        )
        .unwrap();

        assert_eq!(candidate.ufrag(), Some("myuserfrag"));
        assert_eq!(candidate.addr().to_string(), "1.2.3.4:9876");
    }

    #[test]
    fn spoofed_raddr() {
        let local_addr = "7.8.9.0:2345".parse().unwrap();
        let socket_addr = "1.2.3.4:9876".parse().unwrap();
        let base_addr = "5.6.7.8:4321".parse().unwrap();

        let host = Candidate::host(socket_addr, Protocol::Udp).unwrap();
        assert!(host.raddr().is_none());

        // We're not picky on the exact choice, but it must not be the private base
        let relay = Candidate::relayed(socket_addr, local_addr, Protocol::Udp).unwrap();
        assert!(relay.raddr().is_some());
        let srflx = Candidate::server_reflexive(socket_addr, base_addr, Protocol::Udp).unwrap();
        assert!(srflx.raddr().is_some_and(|raddr| raddr != base_addr));

        let prflx = Candidate::peer_reflexive(
            Protocol::Udp,
            socket_addr,
            base_addr,
            1000,
            None,
            "ufrag".into(),
        );
        assert!(prflx.raddr().is_none());
    }

    #[test]
    fn bad_candidate() {
        let s = "candidate:12344 bad value";
        assert!(Candidate::from_sdp_string(s).is_err());
    }

    #[test]
    fn tcp_candidates_sanity() {
        let socket_addr = "1.2.3.4:9876".parse().unwrap();
        let candidates = [
            Candidate::host(socket_addr, Protocol::Tcp).unwrap(),
            Candidate::builder()
                .tcp()
                .host(socket_addr)
                .build()
                .unwrap(),
            Candidate::builder()
                .tcp()
                .host(socket_addr)
                .tcptype(TcpType::Passive)
                .build()
                .unwrap(),
            Candidate::builder()
                .ssl_tcp()
                .host(socket_addr)
                .tcptype(TcpType::Active)
                .build()
                .unwrap(),
            Candidate::builder()
                .tls()
                .host(socket_addr)
                .tcptype(TcpType::So)
                .build()
                .unwrap(),
        ];

        assert!(!candidates[0].to_sdp_string().contains("tcptype"));
        assert!(!candidates[1].to_sdp_string().contains("tcptype"));
        assert!(candidates[2].to_sdp_string().contains("tcptype passive"));
        assert!(candidates[3].to_sdp_string().contains("tcptype active"));
        assert!(candidates[4].to_sdp_string().contains("tcptype so"));
    }

    #[test]
    fn lexical_ordering_of_sdp_is_follows_priority() {
        let mut candidates = Vec::from([
            host("1.1.1.1:0"),
            host("2.2.2.2:0"),
            srflx("3.3.3.3:0", "4.4.4.4:0"),
            srflx("5.5.5.5:0", "6.6.6.6:0"),
            relay("8.8.8.8:0", "9.9.9.9:0"),
            relay("7.7.7.7:0", "9.9.9.9:0"),
        ]);
        candidates.sort();

        assert!(candidates[0].contains("relay"));
        assert!(candidates[1].contains("relay"));
        assert!(candidates[2].contains("srflx"));
        assert!(candidates[3].contains("srflx"));
        assert!(candidates[4].contains("host"));
        assert!(candidates[5].contains("host"));
    }

    #[test]
    fn srflx_candidate_disallows_mixed_ip_versions() {
        let error = Candidate::server_reflexive(
            "10.0.0.1:1000".parse().unwrap(),
            "[::1]:1000".parse().unwrap(),
            "udp",
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "ICE bad candidate: addr and base are different IP versions"
        );
    }

    fn host(socket: &str) -> String {
        Candidate::host(socket.parse().unwrap(), "udp")
            .unwrap()
            .to_sdp_string()
    }

    fn srflx(addr: &str, base: &str) -> String {
        Candidate::server_reflexive(addr.parse().unwrap(), base.parse().unwrap(), "udp")
            .unwrap()
            .to_sdp_string()
    }

    fn relay(addr: &str, local: &str) -> String {
        Candidate::relayed(addr.parse().unwrap(), local.parse().unwrap(), "udp")
            .unwrap()
            .to_sdp_string()
    }

    #[test]
    fn builder_matches_api_host() {
        let addr = "1.2.3.4:1234".parse().unwrap();

        // UDP Host
        let api = Candidate::host(addr, Protocol::Udp).unwrap();
        let builder = Candidate::builder().udp().host(addr).build().unwrap();
        assert_eq!(api, builder);

        // TCP Host
        let api = Candidate::host(addr, Protocol::Tcp).unwrap();
        let builder = Candidate::builder().tcp().host(addr).build().unwrap();
        assert_eq!(api, builder);
    }

    #[test]
    fn builder_matches_api_server_reflexive() {
        let addr = "95.1.1.1:5000".parse().unwrap();
        let base = "192.168.1.50:5000".parse().unwrap();

        let api = Candidate::server_reflexive(addr, base, Protocol::Udp).unwrap();
        let builder = Candidate::builder()
            .udp()
            .server_reflexive(addr, base)
            .build()
            .unwrap();

        assert_eq!(api, builder);
    }

    #[test]
    fn builder_matches_api_relayed() {
        let addr = "1.2.3.4:3478".parse().unwrap(); // TURN relay addr
        let local = "192.168.1.50:5000".parse().unwrap(); // Local interface

        let api = Candidate::relayed(addr, local, Protocol::Udp).unwrap();
        let builder = Candidate::builder()
            .udp()
            .relayed(addr, local)
            .build()
            .unwrap();

        assert_eq!(api, builder);
    }

    #[test]
    fn builder_consistency_errors() {
        let v4 = "1.2.3.4:1234".parse().unwrap();
        let v6 = "[2001:db8::1]:1234".parse().unwrap();

        // API returns error on mixed versions
        let api_err = Candidate::server_reflexive(v4, v6, Protocol::Udp);
        // Builder should return same error type
        let builder_err = Candidate::builder().udp().server_reflexive(v4, v6).build();

        assert!(api_err.is_err());
        assert!(builder_err.is_err());
    }
}
