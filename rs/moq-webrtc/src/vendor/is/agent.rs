use std::collections::{HashSet, VecDeque};
use std::fmt;
use std::net::SocketAddr;
use std::ops::RangeInclusive;
use std::panic::{RefUnwindSafe, UnwindSafe};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::Sha1HmacProvider;
use crate::preference::default_local_preference;
use crate::stun::{Class as StunClass, Method as StunMethod, StunTiming};
use crate::stun::{StunMessage, StunPacket, TransId};
use str0m_proto::{DATAGRAM_MTU_TARGET, DATAGRAM_MTU_WARN, Id, Transmit};
use str0m_proto::{NonCryptographicRng, Pii, Protocol};

use crate::candidate::{Candidate, CandidateKind};
use crate::pair::{CandidatePair, CheckState, PairId};

/// Handles the ICE protocol for a given peer.
///
/// Each connection between two peers corresponds to one [`IceAgent`] on either end.
/// To form connections to multiple peers, a peer needs to create a dedicated [`IceAgent`] for
/// each one.
#[derive(Debug)]
pub struct IceAgent {
    /// Last time handle_timeout run (paced by timing_advance).
    ///
    /// This drives the state forward.
    last_now: Option<Instant>,

    /// Timing advance (Ta) value.
    ///
    /// ICE agents SHOULD use a default Ta value, 50 ms, but MAY use another
    /// value based on the characteristics of the associated data.
    timing_advance: Duration,

    /// Whether this agent is operating as ice-lite.
    /// ice-lite is a minimal version of the ICE specification, intended for servers
    /// running on a public IP address. ice-lite requires the media server to only answer
    /// incoming STUN binding requests and acting as a controlled entity in the ICE
    /// process itself.
    ice_lite: bool,

    // The default limit of candidate pairs for the checklist set is 100,
    // but the value MUST be configurable.
    max_candidate_pairs: Option<usize>,

    /// Whether we have previously exceeded the `max_candidate_pairs` limit.
    has_exceeded_max_candidate_pairs: bool,

    /// Credentials for this side. Set on init and ice-restart.
    local_credentials: IceCreds,

    /// Credentials for the remote side. Set when we learn about it.
    remote_credentials: Option<IceCreds>,

    /// If this side is controlling or controlled.
    controlling: bool,

    /// Number used in STUN attribute ICE-CONTROLLING and ICE-CONTROLLED.
    /// An ICE agent MUST use the same number for all Binding requests,
    /// for all streams, within an ICE session
    control_tie_breaker: u64,

    /// Current state of the agent.
    state: IceConnectionState,

    /// All local candidates, in the order they are "discovered" (either by
    /// adding explicitly using add_candidate, or via binding/allocation
    /// requests.
    local_candidates: Vec<Candidate>,

    /// All remote candidates, in the order we get to know them.
    remote_candidates: Vec<Candidate>,

    /// The candidate pairs.
    candidate_pairs: Vec<CandidatePair>,

    /// Transmit packet ready to be polled by poll_transmit.
    transmit: VecDeque<Transmit>,

    /// Events ready to be polled by poll_event.
    events: VecDeque<IceAgentEvent>,

    /// Queue of incoming STUN requests we might have to queue up before we receive
    /// the remote_credentials.
    stun_server_queue: VecDeque<StunRequest>,

    /// Remote addresses we have seen traffic appear from. This is used
    /// to dedupe [`IceAgentEvent::DiscoveredRecv`].
    discovered_recv: HashSet<(Protocol, SocketAddr)>,

    /// Currently nominated pair for sending. This is used to evaluate
    /// if we get a better candidate for [`IceAgentEvent::NominatedSend`].
    nominated_send: Option<PairId>,

    /// Statistics counter for the agent.
    stats: IceAgentStats,

    /// SHA1-HMAC provider for STUN message integrity.
    sha1_hmac_provider: &'static dyn Sha1HmacProvider,

    /// The timing configuration for STUN bindings.
    timing_config: StunTiming,

    /// Pluggable calculation of local preference.
    local_preference: LocalPreferenceHolder,

    /// Target MTU (start) and warn threshold (end). Buffer sizing uses the
    /// target; oversized outgoing datagrams above the warn threshold log a warning.
    mtu: RangeInclusive<usize>,
}

/// IceAgent contains only static references to thread-safe traits,
/// so it's safe to use across panic boundaries.
impl UnwindSafe for IceAgent {}
impl RefUnwindSafe for IceAgent {}

// Stupid holder to implement fmt::Debug
struct LocalPreferenceHolder(Arc<dyn LocalPreference>);

/// Trait for pluggable LocalPreference calculation
pub trait LocalPreference: RefUnwindSafe + Send + Sync + 'static {
    /// Calculate the local preference for a candidate.
    ///
    /// The `same_kind` parameter is the number of candidates of the same IP version that
    /// have already been added to the agent.
    ///
    /// The `c` parameter is the candidate to calculate the preference for.
    fn calculate(&self, c: &Candidate, same_kind: usize) -> u32;
}

/// Blanket impl for functions that look like preference calculations.
impl<F> LocalPreference for F
where
    F: Fn(&Candidate, usize) -> u32 + RefUnwindSafe + Send + Sync + 'static,
{
    fn calculate(&self, c: &Candidate, same_kind: usize) -> u32 {
        (self)(c, same_kind)
    }
}

#[derive(Debug)]
struct StunRequest {
    now: Instant,
    proto: Protocol,
    source: SocketAddr,
    destination: SocketAddr,
    trans_id: TransId,
    prio: u32,
    use_candidate: bool,
    remote_ufrag: String,
    ice_controlling: Option<u64>,
    ice_controlled: Option<u64>,
}

impl StunRequest {
    fn role_conflict_tiebreaker(&self, controlling: bool) -> Option<u64> {
        if controlling {
            self.ice_controlling
        } else {
            self.ice_controlled
        }
    }
}

fn role_name(controlling: bool) -> &'static str {
    if controlling {
        "controlling"
    } else {
        "controlled"
    }
}

const REMOTE_PEER_REFLEXIVE_TEMP_FOUNDATION: &str = "tmp_prflx";

/// States the ICE connection can be in.
///
/// More details on connection states can be found in the [ICE RFC][1].
///
/// [1]: https://www.rfc-editor.org/rfc/rfc8445
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IceConnectionState {
    /// The ICE agent is gathering addresses.
    New,

    /// The ICE agent is checking pairs of local and remote candidates against one
    /// another to try to find a compatible match, but has not yet found a pair
    /// which will allow the peer connection to be made. It is possible that
    /// gathering of candidates is also still underway.
    Checking,

    /// A usable pairing of local and remote candidates has been found, and the
    /// connection has been established. The agent is not in `Completed` because
    /// it is still gathering candidates or is still checking candidates against
    /// one another looking for a better connection to use.
    Connected,

    /// The ICE agent has finished gathering candidates, has checked all pairs
    /// against one another, and has found a working connection.
    Completed,

    /// Connection failed. This is a less stringent test than `failed` and may trigger
    /// intermittently and resolve just as spontaneously on less reliable networks,
    /// or during temporary disconnections. When the problem resolves, the connection
    /// may return to the connected state.
    Disconnected,
    //
    // NB: The failed and closed state doesn't really have a mapping in this implementation.
    //     We never end trickle ice and it's always possible to "come back" if more remote
    //     candidates are added.
    //
    // The ICE candidate has checked all candidates pairs against one another and has
    // failed to find compatible matches.
    // Failed,
    // The ICE agent has shut down and is no longer handling requests.
    // Closed,
}

impl IceConnectionState {
    /// Tells if this state is a connected state.
    pub fn is_connected(&self) -> bool {
        use IceConnectionState::*;
        matches!(self, Connected | Completed)
    }

    /// Tells if this state is a disconnected state.
    pub fn is_disconnected(&self) -> bool {
        *self == IceConnectionState::Disconnected
    }
}

/// Credentials for STUN packages.
///
/// By matching IceCreds in STUN to SDP, we know which STUN belongs to which Peer.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct IceCreds {
    /// From a=ice-ufrag
    pub ufrag: String,
    /// From a=ice-pwd
    pub pass: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IceAgentStats {
    pub bind_request_sent: u64,
    pub bind_success_recv: u64,
    pub bind_request_recv: u64,
    pub discovered_recv_count: u64,
    pub nomination_send_count: u64,
}

/// Events from an [`IceAgent`].
#[derive(Debug, PartialEq, Eq)]
pub enum IceAgentEvent {
    /// The agent restarted (or started).
    IceRestart(IceCreds),

    /// Connection state changed.
    ///
    /// This is mostly for show since the actual addresses to use will be
    /// communicated in `PossibleRemote` and `NominatedLocal`.
    IceConnectionStateChange(IceConnectionState),

    /// A possible remote socket for the peer.
    ///
    /// The application should associate this with the peer. There will
    /// be more than one of these, and traffic might eventually come in
    /// on any of them.
    ///
    /// For each ICE restart, the app will only receive unique addresses once.
    DiscoveredRecv {
        /// The protocol to use for the socket.
        proto: Protocol,
        /// The remote socket to look out for.
        source: SocketAddr,
    },

    /// A nominated local and remote socket for sending data.
    ///
    /// As opposed to the ICE spec, we can send this multiple times without
    /// requiring an ICE restart. The application should always use the values
    /// of the last emitted event to send data.
    NominatedSend {
        /// The protocol to use for the socket.
        proto: Protocol,
        /// The local socket address to send datagrams from.
        ///
        /// This will correspond to some local address added to
        /// [`IceAgent::add_local_candidate`].
        source: SocketAddr,
        /// The remote address to send datagrams to.
        destination: SocketAddr,
    },
}

impl IceCreds {
    /// Creates a new instance of `IceCreds` with random values for the username fragment and password.
    pub fn new() -> Self {
        // Username Fragment and Password:  Values used to perform connectivity
        // checks.  The values MUST be unguessable, with at least 128 bits of
        // random number generator output used to generate the password, and
        // at least 24 bits of output to generate the username fragment.
        let ufrag = Id::<16>::random().to_string();
        let pass = Id::<22>::random().to_string();
        IceCreds { ufrag, pass }
    }
}

impl IceAgent {
    /// Create a new [`IceAgent`] using the default SHA1-HMAC provider.
    ///
    /// Available when the `sha1` feature is enabled (on by default).
    #[cfg(feature = "sha1")]
    pub fn new(local_credentials: IceCreds) -> Self {
        Self::with_hmac(local_credentials, &crate::DefaultSha1HmacProvider)
    }

    /// Create a new [`IceAgent`] with a specific SHA1-HMAC provider.
    pub fn with_hmac(
        local_credentials: IceCreds,
        sha1_hmac_provider: &'static dyn Sha1HmacProvider,
    ) -> Self {
        IceAgent {
            last_now: None,
            ice_lite: false,
            max_candidate_pairs: None,
            has_exceeded_max_candidate_pairs: false,
            local_credentials,
            remote_credentials: None,
            controlling: false,
            control_tie_breaker: NonCryptographicRng::u64(),
            state: IceConnectionState::New,
            local_candidates: vec![],
            remote_candidates: vec![],
            candidate_pairs: vec![],
            transmit: VecDeque::new(),
            events: VecDeque::new(),
            stun_server_queue: VecDeque::new(),
            discovered_recv: HashSet::new(),
            nominated_send: None,
            stats: IceAgentStats::default(),
            timing_advance: Duration::from_millis(50),
            timing_config: StunTiming::default(),
            local_preference: LocalPreferenceHolder(Arc::new(default_local_preference)),
            sha1_hmac_provider,
            mtu: DATAGRAM_MTU_TARGET..=DATAGRAM_MTU_WARN,
        }
    }

    /// Set the UDP datagram MTU range used for sizing internal buffers (target..=warn).
    pub fn set_mtu(&mut self, mtu: RangeInclusive<usize>) {
        self.mtu = mtu;
    }

    /// Target MTU used for sizing outgoing STUN buffers.
    pub fn mtu(&self) -> usize {
        *self.mtu.start()
    }

    /// Threshold above which an outgoing datagram triggers an MTU warning.
    pub fn mtu_warn(&self) -> usize {
        *self.mtu.end()
    }

    /// Sets the control tie breaker of this agent.
    ///
    /// By default, this is randomly generated and per the ICE spec,
    /// this MUST be the same for the duration of an ICE session.
    pub fn set_control_tie_breaker(&mut self, tie_breaker: u64) {
        self.control_tie_breaker = tie_breaker;
    }

    /// The maximum number of candidate pairs to test.
    ///
    /// Any pairs above this limit will be dropped (worst priority first).
    pub fn set_max_candidate_pairs(&mut self, max: usize) {
        self.max_candidate_pairs = Some(max);
        self.has_exceeded_max_candidate_pairs = false; // Reset the flag.
    }

    /// Whether ice_lite is enabled.
    ///
    /// Default is disabled.
    pub fn ice_lite(&self) -> bool {
        self.ice_lite
    }

    /// Enable or disable ice_lite.
    ///
    /// Default is disabled.
    pub fn set_ice_lite(&mut self, enabled: bool) {
        self.ice_lite = enabled;
    }

    /// Set a new timing advance (Ta) value.
    ///
    /// Ta specifies the minimum increment of time that has to pass between calls to
    /// [`IceAgent::handle_timeout`]s (guided via [`IceAgent::poll_timeout`]).
    ///
    /// Defaults to 50ms.
    pub fn set_timing_advance(&mut self, duration: Duration) {
        self.timing_advance = duration
    }

    /// Local ice credentials.
    pub fn local_credentials(&self) -> &IceCreds {
        &self.local_credentials
    }

    /// Sets the local ice credentials.
    pub fn set_local_credentials(&mut self, r: IceCreds) {
        if self.local_credentials != r {
            debug!("Set local credentials: {:?}", Pii(&r));
            self.local_credentials = r;
        }
    }

    /// Sets the initial STUN **R**etransmission **T**ime**O**ut.
    ///
    /// It defines the initial period of time between transmission of a request
    /// and the first retransmit of that request. The actual RTO doubles with
    /// each retransmit up until the configured maximum RTO.
    ///
    /// Defaults to 250ms.
    pub fn set_initial_stun_rto(&mut self, timeout: Duration) {
        if self.timing_config.initial_rto == timeout {
            return;
        }

        self.timing_config.initial_rto = timeout;

        debug!("initial_rto = {timeout:?}");

        self.bust_candidate_pair_timeout_caches();
    }

    /// Sets the maximum STUN **R**etransmission **T**ime**O**ut.
    ///
    /// It defines the maximum period of time between transmission of a request
    /// and the first retransmit of that request. Once a candidate pair is
    /// successful, this is how often we check that a STUN binding is alive.
    /// As the STUN bindings of a successful candidate pair start to time out,
    /// we probe the binding more often by halfing this value, up until the
    /// maximum number of retransmits before we declare them failed.
    ///
    /// Defaults to 3000ms.
    pub fn set_max_stun_rto(&mut self, timeout: Duration) {
        if self.timing_config.max_rto == timeout {
            return;
        }

        self.timing_config.max_rto = timeout;

        debug!("max_rto = {timeout:?}");

        self.bust_candidate_pair_timeout_caches();
    }

    /// Sets the maximum number of retransmits for STUN messages.
    ///
    /// Defaults to 9.
    pub fn set_max_stun_retransmits(&mut self, num: usize) {
        if self.timing_config.max_retransmits == num {
            return;
        }

        self.timing_config.max_retransmits = num;

        debug!("max_retransmits = {num}");
    }

    /// Sets the local preference calculation.
    ///
    /// This must be used before adding any local candidates.
    pub fn set_local_preference(&mut self, p: impl LocalPreference) {
        self.local_preference = LocalPreferenceHolder(Arc::new(p));
    }

    fn bust_candidate_pair_timeout_caches(&mut self) {
        for pair in self.candidate_pairs.iter_mut() {
            pair.reset_cached_next_attempt_time();
        }
    }

    /// How long we at most tolerate missing replies for a successful candidate pair
    /// before considering it failed.
    pub fn ice_timeout(&self) -> Duration {
        self.timing_config.timeout()
    }

    /// Local ice candidates.
    ///
    /// The candidates have their ufrag filled out to the local credentials.
    pub fn local_candidates(&self) -> impl Iterator<Item = Candidate> + Clone + '_ {
        self.local_candidates
            .iter()
            .filter(|c| !c.discarded())
            .cloned()
    }

    /// Remote ice candidates.
    pub fn remote_candidates(&self) -> impl Iterator<Item = Candidate> + Clone + '_ {
        self.remote_candidates
            .iter()
            .filter(|c| !c.discarded())
            .cloned()
    }

    /// Determines whether any remote candidates match the specified address and
    /// have been verified with a STUN request/response.
    pub fn has_viable_remote_candidate(&self, addr: SocketAddr) -> bool {
        self.candidate_pairs
            .iter()
            .filter(|cand| cand.state() == CheckState::Succeeded)
            .any(|pair| self.remote_candidates[pair.remote_idx()].addr() == addr)
    }

    /// Remote ice credentials.
    pub fn remote_credentials(&self) -> Option<&IceCreds> {
        self.remote_credentials.as_ref()
    }

    /// Sets the remote ice credentials.
    pub fn set_remote_credentials(&mut self, r: IceCreds) {
        if self.remote_credentials.as_ref() != Some(&r) {
            debug!("Set remote credentials: {:?}", Pii(&r));
            self.remote_credentials = Some(r);
        }
    }

    /// Determine whether an equivalent remote candidate is part of a viable candidate pair.
    pub fn find_pair_for_equivalent_remote_candidate(
        &self,
        c: &Candidate,
    ) -> Option<&CandidatePair> {
        self.candidate_pairs.iter().find(|pair| {
            let o = &self.remote_candidates[pair.remote_idx()];
            c.addr() == o.addr()
                && c.base() == o.base()
                && c.proto() == o.proto()
                && c.kind() == o.kind()
                && c.raddr() == o.raddr()
        })
    }

    /// Credentials for STUN.
    ///
    /// The username for the credential is formed by concatenating the
    /// username fragment provided by the peer with the username fragment of
    /// the ICE agent sending the request, separated by a colon (":").
    ///
    /// The password is equal to the password provided by the peer.
    ///
    /// The responses utilize the same usernames and passwords as the requests
    /// (note that the USERNAME attribute is not present in the response).
    ///
    /// ### Panics
    ///
    /// Panics if there are no remote credentials set.
    fn stun_credentials(&self, reply: bool) -> (String, String) {
        let local = &self.local_credentials;

        let (left, right, peer_pass) = if reply {
            ("not_used", "not_used", "not_used")
        } else {
            let peer = self
                .remote_credentials
                .as_ref()
                .expect("Remote ICE credentials");
            (&peer.ufrag[..], &local.ufrag[..], &peer.pass[..])
        };

        let username = format!("{left}:{right}");
        let password = if reply {
            local.pass.clone()
        } else {
            peer_pass.into()
        };

        (username, password)
    }

    /// Whether this side is controlling or controlled.
    #[allow(unused)]
    pub fn controlling(&self) -> bool {
        self.controlling
    }

    /// Set whether we are the controlling side.
    ///
    /// You should not call this function after ICE candidate pair formation
    /// has started, as the controlling state influences candidate prio!
    pub fn set_controlling(&mut self, v: bool) {
        self.controlling = v;
    }

    /// Current ice agent state.
    pub fn state(&self) -> IceConnectionState {
        self.state
    }

    /// Stats for the agent.
    ///
    /// Resets on ICE restart.
    #[allow(unused)]
    pub fn stats(&self) -> IceAgentStats {
        self.stats
    }

    /// Adds a local candidate.
    ///
    /// Returns `Some` if the candidate was added and `None` in all other cases.
    /// If the candidate was added, it should be signalled to the remote party.
    ///
    /// Adding loopback addresses or multicast/broadcast addresses causes
    /// an error.
    pub fn add_local_candidate(&mut self, mut c: Candidate) -> Option<&Candidate> {
        let ip = c.addr().ip();

        if self.ice_lite {
            // Reject all non-host candidates.
            if c.kind() != CandidateKind::Host {
                debug!("Reject non-host candidate due to ice-lite mode: {:?}", c);
                return None;
            }
        }

        // Count the number of existing candidates of the same kind.
        let same_kind = self
            .local_candidates
            .iter()
            .filter(|v| v.kind() == c.kind())
            .filter(|v| v.addr().is_ipv6() == ip.is_ipv6())
            .count();

        // Delegate local preference calculation to pluggable algo.
        let pref = self.local_preference.0.calculate(&c, same_kind);
        trace!("Calculated local preference: {}", pref);
        c.set_local_preference(pref);

        // "Adopt" any incoming candidate by setting our current ufrag.
        c.set_ufrag(&self.local_credentials.ufrag);

        // A candidate is redundant if and only if its transport address and base equal those
        // of another candidate.  The agent SHOULD eliminate the redundant
        // candidate with the lower priority.
        //
        // NB this must be done _after_ set_local_preference(), since the prio() used in the
        // elimination is calculated from that preference.
        let maybe_redundant =
            self.local_candidates.iter_mut().enumerate().find(|(_, v)| {
                v.addr() == c.addr() && v.base() == c.base() && v.proto() == c.proto()
            });

        let local_idx = if let Some((idx, other)) = maybe_redundant {
            if other.discarded() && c.kind() == other.kind() && c.raddr() == other.raddr() {
                debug!("Re-enable previously discarded local: {:?}", other);
                other.set_discarded(false);
                idx
            } else {
                if c.prio() < other.prio() {
                    // The new candidate is not better than what we already got.
                    debug!(
                        "Reject redundant candidate, current: {:?} rejected: {:?}",
                        Pii(&other),
                        Pii(&c)
                    );
                    return None;
                }

                // Stop using the current candidate in favor of the new one.
                debug!(
                    "Replace redundant candidate, current: {:?} replaced with: {:?}",
                    Pii(&other),
                    Pii(&c)
                );
                other.set_discarded(true);
                self.discard_candidate_pairs_by_local(idx);

                let new_idx = self.local_candidates.len();

                debug!("Add local candidate ({new_idx}): {:?}", Pii(&c));
                self.local_candidates.push(c);

                new_idx
            }
        } else {
            let new_idx = self.local_candidates.len();

            debug!("Add local candidate ({new_idx}): {:?}", Pii(&c));
            self.local_candidates.push(c);

            new_idx
        };

        // These are the indexes of the remote candidates this candidate should be paired with.
        let remote_idxs: Vec<_> = self
            .remote_candidates
            .iter()
            .enumerate()
            .filter(|(_, v)| !v.discarded() && v.addr().is_ipv4() == ip.is_ipv4())
            .map(|(i, _)| i)
            .collect();

        // We always run in trickle ice mode.
        //
        // https://www.rfc-editor.org/rfc/rfc8838.html#section-10
        // A Trickle ICE agent MUST NOT pair a local candidate until it has been trickled
        // to the remote party.
        //
        // TODO: The trickle ice spec is strange. What does it mean "has been trickled to the
        // remote party"? Since we don't get a confirmation that the candidate has been received
        // by the remote party, whether we form local pairs directly or later seems irrelevant.
        self.form_pairs(&[local_idx], &remote_idxs);

        // We specifically use indexing here instead of `.get`.
        // If the index is wrong, this is a bug!
        Some(&self.local_candidates[local_idx])
    }

    /// Adds a remote candidate.
    ///
    /// Returns `false` if the candidate was not added because it is redundant.
    /// Adding loopback addresses or multicast/broadcast addresses causes
    /// an error.
    pub fn add_remote_candidate(&mut self, mut c: Candidate) {
        // This is a a:rtcp-mux-only implementation. The only component
        // we accept is 1 for RTP.
        if c.component_id() != 1 {
            debug!("Reject candidate for component other than 1: {:?}", Pii(&c));
            return;
        }

        if let Some(creds) = &self.remote_credentials {
            if let Some(ufrag) = c.ufrag() {
                if ufrag != creds.ufrag {
                    debug!(
                        "Reject candidate with ufrag mismatch: {} != {}",
                        Pii(&ufrag),
                        Pii(&creds.ufrag)
                    );
                    return;
                }
            }
        }

        // After we accepted the ufrag, don't keep this around since it will look
        // confusing inspecting the state.
        c.clear_ufrag();

        let existing_pair = self.find_pair_for_equivalent_remote_candidate(&c);
        let existing_candidate =
            existing_pair.map(|p| (p.remote_idx(), &self.remote_candidates[p.remote_idx()]));

        let existing_idx = match existing_candidate {
            Some((_, o)) if !o.discarded() => {
                // Existing non-discarded candidate in viable pair, ignore
                // Discarded candidates and candidates not in a viable pair are handled below
                trace!(
                    "Ignoring candidate({c:?}) that exactly matches existing non-discarded candidate"
                );
                return;
            }
            Some((i, o)) if o.discarded() => Some(i),
            _ => None,
        };

        let existing_prflx = self
            .remote_candidates
            .iter_mut()
            .enumerate()
            .find(|(_, v)| {
                v.foundation() == REMOTE_PEER_REFLEXIVE_TEMP_FOUNDATION
                    && v.kind() == CandidateKind::PeerReflexive
                    && v.addr() == c.addr()
            });

        let ipv4 = c.addr().is_ipv4();

        let remote_idx = if let Some((idx, existing)) = existing_prflx {
            // If any subsequent candidate exchanges contain this peer-reflexive
            // candidate, it will signal the actual foundation for the candidate.
            debug!(
                "Replace peer reflexive candidate, current: {:?} replaced with: {:?}",
                Pii(&existing),
                Pii(&c)
            );
            *existing = c;
            idx
        } else {
            let existing_discarded = existing_idx.and_then(|idx| {
                let o = &mut self.remote_candidates[idx];

                o.discarded().then_some((idx, o))
            });

            if let Some((idx, other)) = existing_discarded {
                debug!("Re-enable previously discarded remote: {:?}", Pii(&other));
                other.set_discarded(false);
                idx
            } else {
                let new_idx = self.remote_candidates.len();

                debug!("Add remote candidate ({new_idx}): {:?}", Pii(&c));
                self.remote_candidates.push(c);

                new_idx
            }
        };

        // These are the indexes of the local candidates this candidate should be paired with.
        let local_idxs: Vec<_> = self
            .local_candidates
            .iter()
            .enumerate()
            .filter(|(_, v)| !v.discarded() && v.addr().is_ipv4() == ipv4)
            .map(|(i, _)| i)
            .collect();

        let remote_idxs = [remote_idx];
        self.form_pairs(&local_idxs, &remote_idxs);
    }

    /// Form pairs given two slices of indexes into the local_candidates and remote_candidates.
    fn form_pairs(&mut self, local_idxs: &[usize], remote_idxs: &[usize]) {
        // Ensure to first update the kinds so any log statements are up-to-date.
        for pair in &mut self.candidate_pairs {
            pair.update_kinds(&self.local_candidates, &self.remote_candidates);
        }

        for local_idx in local_idxs {
            'outer: for remote_idx in remote_idxs {
                let local = &self.local_candidates[*local_idx];
                let remote = &self.remote_candidates[*remote_idx];

                // Candidates in a pair must share the same protocol
                if local.proto() != remote.proto() {
                    continue 'outer;
                }

                let prio =
                    CandidatePair::calculate_prio(self.controlling, remote.prio(), local.prio());
                let mut pair =
                    CandidatePair::new(*local_idx, local.kind(), *remote_idx, remote.kind(), prio);

                trace!("Form pair local: {:?} remote: {:?}", local, remote);

                // The agent prunes each checklist.  This is done by removing a
                // candidate pair if it is redundant with a higher-priority candidate
                // pair in the same checklist.  Two candidate pairs are redundant if
                // their local candidates have the same base and their remote candidates
                // are identical.

                for (check_idx, check) in self.candidate_pairs.iter().enumerate() {
                    let check_local = check.local_candidate(&self.local_candidates);
                    let check_remote = check.remote_candidate(&self.remote_candidates);

                    let redundant = local.base() == check_local.base()
                        && remote.addr() == check_remote.addr()
                        && local.proto() == check_local.proto()
                        && remote.proto() == check_remote.proto();

                    if redundant {
                        if check.prio() >= pair.prio() {
                            // skip this new pair since there is a redundant pair already in the
                            // list with higher/equal priority.
                            debug!(
                                "Reject redundant pair, current: {:?} rejected: {:?}",
                                Pii(&check),
                                Pii(&pair)
                            );
                        } else {
                            // replace the existing candidate pair, since the new one got a higher prio.
                            pair.copy_nominated_and_success_state(&self.candidate_pairs[check_idx]);

                            debug!(
                                "Replace redundant pair, current: {:?} replaced with: {:?}",
                                Pii(&check),
                                Pii(&pair)
                            );

                            if self.ice_lite {
                                debug!("Retain incoming binding requests for pair");
                                pair.copy_remote_binding_requests(&self.candidate_pairs[check_idx]);
                            }

                            self.candidate_pairs[check_idx] = pair;
                        }

                        // There can only be one candidate pair per local base / remote addr.
                        // Since we found that redundant entry, there's no point in checking further
                        // candidate pairs.
                        continue 'outer;
                    }
                }

                debug!("Add new pair {:?}", Pii(&pair));

                // This is not a redundant pair, add it.
                self.candidate_pairs.push(pair);
            }
        }

        // NB it would be nicer to have BTreeSet, but that makes it impossible to
        // get mut references to the elements in the list.
        self.candidate_pairs.sort();

        // an ICE agent MUST limit the total number of connectivity checks
        // the agent performs across all checklists in the checklist set.
        // This is done by limiting the total number of candidate pairs in the
        // checklist set. The default limit of candidate pairs for the checklist
        // set is 100, but the value MUST be configurable.
        //
        // TODO: How does this work with trickle ice?
        let max = self.max_candidate_pairs.unwrap_or(100);

        let num_pairs = self.candidate_pairs.len();
        if num_pairs > max && !self.has_exceeded_max_candidate_pairs {
            warn!(%max, %num_pairs, "Exceeded max number of candidate pairs");
            self.has_exceeded_max_candidate_pairs = true;
        }

        while self.candidate_pairs.len() > max {
            let pair = self.candidate_pairs.pop();
            debug!("Remove overflow pair {:?}", Pii(&pair));
        }
    }

    /// Invalidate a candidate and remove it from the connection.
    ///
    /// This is done for host candidates disappearing due to changes in the network
    /// interfaces like a WiFi disconnecting or changing IPs.
    ///
    /// It can also be used to invalidate _remote_ candidates, i.e. if the remote
    /// has signalled us that they have invalidated one of their candidates.
    ///
    /// Returns `true` if the candidate was found and invalidated.
    pub fn invalidate_candidate(&mut self, c: &Candidate) -> bool {
        if let Some((idx, other)) = self.local_candidates.iter_mut().enumerate().find(|(_, v)| {
            v.addr() == c.addr()
                && v.base() == c.base()
                && v.raddr() == c.raddr()
                && v.kind() == c.kind()
                && v.proto() == c.proto()
        }) {
            if !other.discarded() {
                debug!("Local candidate to discard {:?}", Pii(&other));
                other.set_discarded(true);
                self.discard_candidate_pairs_by_local(idx);
                return true;
            }
        }

        if let Some((idx, other)) = self
            .remote_candidates
            .iter_mut()
            .enumerate()
            .find(|(_, v)| {
                v.addr() == c.addr()
                    && v.base() == c.base()
                    && v.raddr() == c.raddr()
                    && v.kind() == c.kind()
                    && v.proto() == c.proto()
            })
        {
            if !other.discarded() {
                debug!("Remote candidate to discard {:?}", Pii(&other));
                other.set_discarded(true);
                self.discard_candidate_pairs_by_remote(idx);
                return true;
            }
        }

        debug!("No local or remote candidate found: {:?}", Pii(&c));
        false
    }

    /// Restart ICE.
    ///
    /// This is useful when detecting a change in network interfaces, such as
    /// current session running off a 4G, and we connect to a WiFi. The session
    /// should continue sending data over the 4G until we redone the ICE gathering
    /// process.
    #[allow(unused)]
    pub fn ice_restart(&mut self, local_credentials: IceCreds, keep_local_candidates: bool) {
        // An ICE agent MAY restart ICE for existing data streams.  An ICE
        // restart causes all previous states of the data streams, excluding the
        // roles of the agents, to be flushed.  The only difference between an
        // ICE restart and a brand new data session is that during the restart,
        // data can continue to be sent using existing data sessions, and a new
        // data session always requires the roles to be determined.

        self.remote_credentials = None;
        self.remote_candidates.clear();
        self.candidate_pairs.clear();
        self.has_exceeded_max_candidate_pairs = false;
        self.transmit.clear();
        self.events.clear();
        self.discovered_recv.clear();

        if keep_local_candidates {
            // If we're keeping the candidates, we must update the ufrag to the new credentials.
            // This is so anyone inspecting `.local_candidates()` will get the correct ufrag.
            for c in &mut self.local_candidates {
                c.set_ufrag(&local_credentials.ufrag)
            }
        } else {
            self.local_candidates.clear();
        }

        self.local_credentials = local_credentials;

        self.emit_event(IceAgentEvent::IceRestart(self.local_credentials.clone()));
        self.set_connection_state(IceConnectionState::Checking, "ice restart");
    }

    /// Re-create all candidate pairs from the current candidates.
    ///
    /// This is similar to an ICE restart but also retains all remote candidates.
    pub fn recreate_candidate_pairs(&mut self) {
        self.candidate_pairs.clear();

        let local_idxs: Vec<_> = self
            .local_candidates
            .iter()
            .enumerate()
            .filter(|(_, v)| !v.discarded())
            .map(|(i, _)| i)
            .collect();
        let remote_idxs: Vec<_> = self
            .remote_candidates
            .iter()
            .enumerate()
            .filter(|(_, v)| !v.discarded())
            .map(|(i, _)| i)
            .collect();

        self.form_pairs(&local_idxs, &remote_idxs);
        self.set_connection_state(IceConnectionState::Checking, "recreate candidate pairs");
    }

    /// Discard candidate pairs that contain the candidate identified by a local index.
    fn discard_candidate_pairs_by_local(&mut self, local_idx: usize) {
        trace!("Discard pairs for local candidate index: {:?}", local_idx);
        self.candidate_pairs.retain(|c| c.local_idx() != local_idx);
    }

    /// Discard candidate pairs that contain the candidate identified by a remote index.
    fn discard_candidate_pairs_by_remote(&mut self, remote: usize) {
        trace!("Discard pairs for remote candidate index: {:?}", remote);
        self.candidate_pairs.retain(|c| c.remote_idx() != remote);
    }

    /// Tells whether the message is for this agent instance.
    ///
    /// This is used to multiplex multiple ice agents on a server sharing the same UDP socket.
    ///
    /// For binding requests, if no remote credentials have been set using
    /// `set_remote_credentials`, the remote ufrag is not checked.
    pub fn accepts_message(&self, message: &StunMessage<'_>) -> bool {
        trace!("Check if accepts message: {:?}", message);

        let sha1_hmac =
            |key: &[u8], payloads: &[&[u8]]| self.sha1_hmac_provider.sha1_hmac(key, payloads);

        let do_integrity_check = |is_request: bool| -> bool {
            let (_, password) = self.stun_credentials(is_request);
            let integrity_passed = message.verify(password.as_bytes(), sha1_hmac);

            // The integrity is always the last thing we check
            if integrity_passed {
                trace!("Message accepted");
            } else {
                trace!("Message rejected, integrity check failed");
            }
            integrity_passed
        };

        let method = message.method();
        let class = message.class();
        match (method, class) {
            (StunMethod::Binding, StunClass::Indication) => {
                // https://datatracker.ietf.org/doc/html/rfc8489#section-6.3.2
                // An Indication can be safely ignored, its purpose is to refresh NATs in the
                // network path. Some clients MAY omit USERNAME attribute.
                false
            }
            (StunMethod::Binding, StunClass::Request) => {
                // The username for the credential is formed by concatenating the
                // username fragment provided by the peer with the username fragment of
                // the ICE agent sending the request, separated by a colon (":").
                // The existence of this username is checked in the STUN parser.
                let (local, remote) = message.split_username().unwrap();

                let local_creds = self.local_credentials();
                if local != local_creds.ufrag {
                    trace!(
                        "Message rejected, local user mismatch: {} != {}",
                        local, local_creds.ufrag
                    );
                    return false;
                }

                if let Some(remote_creds) = &self.remote_credentials {
                    if remote != remote_creds.ufrag {
                        trace!(
                            "Message rejected, remote user mismatch: {} != {}",
                            remote, remote_creds.ufrag
                        );
                        return false;
                    }
                }

                do_integrity_check(true)
            }
            (StunMethod::Binding, StunClass::Success | StunClass::Failure) => {
                let belongs_to_a_candidate_pair = self
                    .candidate_pairs
                    .iter()
                    .any(|pair| pair.binding_attempt(message.trans_id()).is_some());

                if !belongs_to_a_candidate_pair {
                    trace!(
                        "Message rejected, transaction ID does not belong to any of our candidate pairs"
                    );
                    return false;
                }

                do_integrity_check(false)
            }
            (StunMethod::Binding, StunClass::Unknown) => {
                // Without a known class, it's impossible to know how to validate the message
                trace!("Message rejected, unknown STUN class");
                false
            }
            (StunMethod::Unknown, _) => {
                // Without a known method, it's impossible to know how to validate the message
                trace!("Message rejected, unknown STUN method");
                false
            }
            (
                StunMethod::Allocate
                | StunMethod::Refresh
                | StunMethod::Send
                | StunMethod::Data
                | StunMethod::CreatePermission
                | StunMethod::ChannelBind,
                _,
            ) => {
                // Unexpected TURN related message
                trace!("Message rejected, TURN method({method:?}) unexpected in this context");
                false
            }
        }
    }

    /// Handles an incoming STUN message.
    ///
    /// Will not be used if [`IceAgent::accepts_message`] returns false.
    pub fn handle_packet(&mut self, now: Instant, packet: StunPacket) -> bool {
        trace!("Handle receive: {:?}", &packet.message);

        // Regardless of whether we have remote_creds at this point, we can
        // at least check the message integrity.
        if !self.accepts_message(&packet.message) {
            debug!("Message not accepted");
            return false;
        }

        if packet.message.is_binding_request() {
            self.stun_server_handle_message(now, &packet);
        } else if packet.message.is_successful_binding_response() {
            self.stun_client_handle_response(now, packet.message);
        } else if packet.message.is_failed_binding_response() {
            self.stun_client_handle_error(packet.message);
        }

        self.emit_event(IceAgentEvent::DiscoveredRecv {
            proto: packet.proto,
            source: packet.source,
        });

        // TODO handle unsuccessful responses.

        true
    }

    /// Provide the current time to the [`IceAgent`].
    ///
    /// Typically, you will want to call [`IceAgent::poll_timeout`] and "wake-up"
    /// the agent once that time is reached.
    pub fn handle_timeout(&mut self, now: Instant) {
        // This happens exactly once because evaluate_state() below will
        // switch away from New -> Checking.
        if self.state == IceConnectionState::New {
            self.emit_event(IceAgentEvent::IceRestart(self.local_credentials.clone()));
        }

        self.evaluate_state(now);

        // First we try to empty the queue of saved STUN requests.
        if self.remote_credentials.is_some() {
            let queue = &mut self.stun_server_queue;

            // No need hanging on to very old requests.
            while let Some(peek) = queue.front() {
                if now - peek.now >= self.timing_config.timeout() {
                    let r = queue.pop_front();
                    trace!("Drop too old enqueued STUN request: {:?}", r.unwrap());
                } else {
                    break;
                }
            }

            if let Some(req) = self.stun_server_queue.pop_front() {
                debug!("Handle enqueued STUN request: {:?}", Pii(&req));
                self.stun_server_handle_request(req);
                return;
            }
        }

        self.last_now = Some(now);

        self.evaluate_nomination();

        // prune failed candidates.
        let mut any_pruned = false;
        self.candidate_pairs.retain(|p| {
            let keep = if self.ice_lite {
                p.is_ice_lite_alive(now)
            } else {
                p.is_still_possible(now, &self.timing_config)
            };
            if !keep {
                debug!("Remove failed pair: {:?}", Pii(&p));
                any_pruned = true;
            }
            keep
        });
        if any_pruned {
            self.evaluate_nomination();
            self.evaluate_state(now);
        }

        if self.remote_credentials.is_none() {
            trace!("Stop timeout due to missing remote credentials");
            return;
        }

        if self.ice_lite {
            // Remote binding request time is the timestamp in the CandidatePair that
            // is used to decide whether something is timed out or not. We need all
            // pairs to have this time set, so that pairs that don't receive any
            // STUN binding requests eventually times out.
            for p in &mut self.candidate_pairs {
                if p.remote_binding_request_time().is_none() {
                    p.increase_remote_binding_requests(now);
                }
            }

            trace!("Stop timeout since ice-lite do no checks");
            return;
        }

        // when do we need to handle the next candidate pair?
        let next = self
            .candidate_pairs
            .iter_mut()
            .enumerate()
            .map(|(i, c)| (i, c.next_binding_attempt(now, &self.timing_config)))
            .min_by_key(|(_, t)| *t);

        if let Some((idx, deadline)) = next {
            if now >= deadline {
                let pair = &self.candidate_pairs[idx];
                trace!("Handle next triggered pair: {:?}", pair);
                self.stun_client_binding_request(now, idx);
            } else {
                // trace!("Next triggered pair is in the future: {:?}", deadline - now);
            }
        }
    }

    /// Poll for the next datagram to send.
    pub fn poll_transmit(&mut self) -> Option<Transmit> {
        let x = self.transmit.pop_front();
        if let Some(x) = &x {
            if x.contents.len() > self.mtu_warn() {
                warn!("ICE above MTU {}: {}", self.mtu_warn(), x.contents.len());
            }
            trace!("Poll transmit: {:?}", x);
        }
        x
    }

    /// Poll for the next time to call [`IceAgent::handle_timeout`].
    ///
    /// For optimal performance, you should call this every time the [`IceAgent`]s state changes.
    /// For example, after you call [`IceAgent::add_local_candidate`] or [`IceAgent::add_remote_candidate`].
    ///
    /// Returns `None` until the first ever `handle_timeout` is called.
    pub fn poll_timeout(&mut self) -> Option<Instant> {
        // if we never called handle_timeout, there will be no current time.
        let last_now = self.last_now?;

        let has_request = !self.stun_server_queue.is_empty();
        let has_transmit = !self.transmit.is_empty();

        // We must empty the queued replies or stuff to send as soon as possible.
        if has_request || has_transmit {
            return Some(last_now + self.timing_advance);
        }

        // when do we need to handle the next candidate pair?
        let maybe_next = if self.ice_lite {
            // ice-lite doesn't do checks.
            None
        } else {
            self.candidate_pairs
                .iter_mut()
                .map(|c| c.next_binding_attempt(last_now, &self.timing_config))
                .min()
        };

        // Time must advance with at least Ta.
        let next = if let Some(next) = maybe_next {
            if next < last_now + self.timing_advance {
                last_now + self.timing_advance
            } else {
                next
            }
        } else {
            // IDLE for a while.
            last_now + Duration::from_secs(3)
        };

        Some(next)
    }

    fn emit_event(&mut self, event: IceAgentEvent) {
        if let IceAgentEvent::DiscoveredRecv { proto, source } = event {
            if !self.discovered_recv.insert((proto, source)) {
                // we already dispatched this discovered
                return;
            }
            self.stats.discovered_recv_count += 1;
        } else if matches!(event, IceAgentEvent::NominatedSend { .. }) {
            self.stats.nomination_send_count += 1;
        }

        trace!("Enqueueing event: {:?}", event);
        self.events.push_back(event);
    }

    /// Return a pending [`IceAgentEvent`] from this agent.
    pub fn poll_event(&mut self) -> Option<IceAgentEvent> {
        let x = self.events.pop_front();
        if x.is_some() {
            trace!("Poll event: {:?}", x);
        }
        x
    }

    fn stun_server_handle_message(&mut self, now: Instant, packet: &StunPacket) {
        let message = &packet.message;
        let prio = message
            .prio()
            // this should be guarded in the parsing
            .expect("STUN request prio");
        let use_candidate = message.use_candidate();

        if use_candidate {
            trace!("Binding request sent USE-CANDIDATE");
        }

        let trans_id = message.trans_id();

        // The existence of USERNAME is checked by the STUN parser.
        let (_, remote_ufrag) = message.split_username().unwrap();

        // Because we might have to delay stun requests until we receive the remote
        // credentials, we extract all relevant bits of information so it can be owned.
        let req = StunRequest {
            now,
            proto: packet.proto,
            source: packet.source,
            destination: packet.destination,
            trans_id,
            prio,
            use_candidate,
            remote_ufrag: remote_ufrag.into(),
            ice_controlling: message.ice_controlling(),
            ice_controlled: message.ice_controlled(),
        };

        if self.remote_credentials.is_some() {
            self.stun_server_handle_request(req);
        } else {
            debug!(
                "Enqueue STUN request due to missing remote credentials: {:?}",
                Pii(&req)
            );

            let queue = &mut self.stun_server_queue;

            // It is possible (and in fact very likely) that the
            // initiating agent will receive a Binding request prior to receiving
            // the candidates from its peer.
            queue.push_back(req);

            // This is some denial-of-service attack protection.
            while queue.len() > 100 {
                let r = queue.pop_front();
                debug!("Remove overflow STUN request {:?}", Pii(&r));
            }

            // If this happens, the agent MUST
            // immediately generate a response.  The agent has sufficient
            // information at this point to generate the response; the password from
            // the peer is not required.

            // TODO: The spec seems to indicate we can generate a reply, but that seems
            // to fly in the face of logic. A reply requires a fingerprint message integrity
            // and we can't construct that until we get the remote password.
        }
    }

    fn stun_server_handle_request(&mut self, req: StunRequest) {
        let remote_creds = self.remote_credentials.as_ref().expect("Remote ICE creds");
        if req.remote_ufrag != remote_creds.ufrag {
            // this check can be delayed due to receiving STUN bind requests before we
            // get the exchange on the signal level.
            debug!(
                "STUN request rejected, remote user mismatch (enqueued): {} != {}",
                Pii(&req.remote_ufrag),
                Pii(&remote_creds.ufrag)
            );
            return;
        }

        // Drop requests that did not arrive on one of our local candidates
        // before doing anything else. We must not mutate state, send replies
        // (including 487 Role Conflict) or create peer-reflexive candidates
        // for traffic that isn't actually directed at one of our interfaces.
        let local_idx = match self.local_candidates.iter().position(|v| {
            matches!(v.kind(), CandidateKind::Host | CandidateKind::Relayed)
                && v.addr() == req.destination
                && v.proto() == req.proto
        }) {
            Some(i) => i,
            None => {
                // Receiving traffic for an IP address that neither is a HOST nor RELAY
                // is most likely a configuration fault where the user forgot to add a
                // candidate for the local interface. We are network-connected application
                // so we need to handle this gracefully: Log a message and discard the packet.
                debug!(
                    "Discarding STUN request on unknown interface: {}",
                    Pii(req.destination)
                );
                return;
            }
        };

        // Role conflict detection: if the peer claims the same role as us,
        // the agent with the higher tiebreaker keeps its role and the loser yields.
        if let Some(peer_tb) = req.role_conflict_tiebreaker(self.controlling) {
            debug!(
                "Role conflict on {} role: ours={}, peer={}",
                role_name(self.controlling),
                self.control_tie_breaker,
                peer_tb,
            );

            if self.control_tie_breaker >= peer_tb {
                self.send_role_conflict_reply(&req);
                return;
            }

            self.switch_role(!self.controlling);
        }

        if req.use_candidate && self.controlling {
            // the other side is not controlling, and it sent USE-CANDIDATE. that's wrong.
            debug!("STUN request rejected, USE-CANDIDATE when local is controlling");
            return;
        }

        self.stats.bind_request_recv += 1;

        // If the source transport address of the request does not match any
        // existing remote candidates, it represents a new peer-reflexive remote
        // candidate.
        let found_in_remote = self
            .remote_candidates
            .iter()
            .enumerate()
            .filter(|(_, c)| !c.discarded() && c.proto() == req.proto && c.addr() == req.source)
            // We may have multiple candidates with the same address
            // (i.e. host and server-reflexive could be the same).
            .max_by_key(|(_, c)| c.prio());

        let remote_idx = if let Some((idx, _)) = found_in_remote {
            trace!("Remote candidate for STUN request found");
            idx
        } else {
            let maybe_discarded = self
                .remote_candidates
                .iter()
                .any(|c| c.discarded() && c.proto() == req.proto && c.addr() == req.source);

            if maybe_discarded {
                // The remote has been discarded, we do not want to create a
                // peer reflexive in this case.
                trace!("STUN request ignored because remote candidate is discarded");
                return;
            }

            // o  The priority is the value of the PRIORITY attribute in the Binding
            //     request.
            //
            // o  The foundation is an arbitrary value, different from the
            //     foundations of all other remote candidates.  If any subsequent
            //     candidate exchanges contain this peer-reflexive candidate, it will
            //     signal the actual foundation for the candidate.
            let c = Candidate::peer_reflexive(
                req.proto,
                req.source,
                req.source,
                req.prio,
                // TODO: REMOTE_PEER_REFLEXIVE_TEMP_FOUNDATION should probably have
                // a counter to really make it "different from the foundations of
                // all other remote candidates".
                // In practice it might no matter since we don't do the frozen-waiting
                // dance for candidate pairs.
                Some(REMOTE_PEER_REFLEXIVE_TEMP_FOUNDATION.into()),
                self.local_credentials.ufrag.clone(),
            );

            debug!(
                "Created peer reflexive remote candidate from STUN request: {:?}",
                Pii(&c)
            );

            // This candidate is added to the list of remote candidates.  However,
            // the ICE agent does not pair this candidate with any local candidates.
            self.remote_candidates.push(c);

            self.remote_candidates.len() - 1
        };

        let maybe_pair = self
            .candidate_pairs
            .iter_mut()
            .find(|p| p.local_idx() == local_idx && p.remote_idx() == remote_idx);

        if let Some(pair) = maybe_pair {
            // When the pair is already on the checklist:
            trace!("Found existing pair for STUN request: {:?}", pair);

            // TODO: The spec has all these ideas about resetting to Waiting state
            // for the candidate pair. I think that's to do speeding up the triggered
            // checks if a nomination comes through. It doesn't seem to make much
            // sense in this implementation, where a nomination jumps the queue.
            // https://datatracker.ietf.org/doc/html/rfc8445#section-7.3.1.4
        } else {
            // If the pair is not already on the checklist:
            let local = &self.local_candidates[local_idx];
            let remote = &self.remote_candidates[remote_idx];

            if local.discarded() {
                return; // Ignore STUN requests to discarded candidates
            }

            let prio = CandidatePair::calculate_prio(self.controlling, remote.prio(), local.prio());

            // *  Its state is set to Waiting. (this is the default)
            // *  The pair is inserted into the checklist based on its priority.
            // *  The pair is enqueued into the triggered-check queue.
            let pair = CandidatePair::new(local_idx, local.kind(), remote_idx, remote.kind(), prio);

            debug!("Created new pair for STUN request: {:?}", Pii(&pair));

            self.candidate_pairs.push(pair);
            self.candidate_pairs.sort();
        }

        let pair = self
            .candidate_pairs
            .iter_mut()
            .find(|p| p.local_idx() == local_idx && p.remote_idx() == remote_idx)
            // unwrap is fine since we have inserted a pair if it was missing.
            .unwrap();

        let local = pair.local_candidate(&self.local_candidates);
        let proto = local.proto();
        let local_addr = local.base();
        let remote = pair.remote_candidate(&self.remote_candidates);
        let remote_addr = remote.addr();

        pair.increase_remote_binding_requests(req.now);

        if self.ice_lite && !self.controlling {
            // ice-lite sends no checks of its own, so this pair can never be
            // promoted by a binding response and stays `Waiting` for its
            // whole life. `has_viable_remote_candidate` therefore never
            // reports the remote, and `Rtc::accepts` refuses the peer's
            // non-STUN traffic until nomination has given us a send address.
            //
            // An answered binding request is not return-routability proof
            // the way a binding *response* is, and this does not claim to
            // be. It is the only validation an ice-lite agent can ever
            // have, and for a controlled agent it grants strictly less
            // than an attacker already has: a forged USE-CANDIDATE request
            // already reaches `pair.nominate()` below and redirects
            // egress, while this only lets ingress reach DTLS/SRTP.
            //
            // A *controlling* ice-lite agent is left alone: there,
            // `CheckState::Succeeded` feeds `evaluate_nomination()` just
            // below, so promoting a pair here would let one forged request
            // with a high PRIORITY choose the send address.
            pair.record_remote_binding_request_success();
        }

        if !self.controlling && !pair.is_nominated() && req.use_candidate {
            // We need to answer a nomination request with a binding request
            // in the other direction.
            //
            // If this is ice-lite, we make it successful straight away.
            pair.nominate(self.ice_lite);
        }

        if self.controlling && pair.state() == CheckState::Succeeded {
            // See if we can nominate something now.
            self.evaluate_nomination();
        }

        let (_, password) = self.stun_credentials(true);

        let reply = StunMessage::binding_reply(req.trans_id, req.source);

        trace!(
            "Send STUN reply: {} -> {} {:?}",
            local_addr, remote_addr, reply
        );

        let mut buf = vec![0_u8; self.mtu()];

        let sha1_hmac =
            |key: &[u8], payloads: &[&[u8]]| self.sha1_hmac_provider.sha1_hmac(key, payloads);
        let n = reply
            .to_bytes(Some(password.as_bytes()), &mut buf, sha1_hmac)
            .expect("IO error writing STUN reply");
        buf.truncate(n);

        let trans = Transmit {
            proto,
            source: local_addr,
            destination: remote_addr,
            contents: buf.into(),
        };

        self.transmit.push_back(trans);
    }

    fn send_role_conflict_reply(&mut self, req: &StunRequest) {
        let (_, password) = self.stun_credentials(true);

        let reply = StunMessage::binding_role_conflict_reply(req.trans_id);

        debug!(
            "Reply 487 Role Conflict (keep {} role): {} -> {}",
            role_name(self.controlling),
            req.destination,
            req.source,
        );

        let mut buf = vec![0_u8; self.mtu()];

        let sha1_hmac =
            |key: &[u8], payloads: &[&[u8]]| self.sha1_hmac_provider.sha1_hmac(key, payloads);
        let n = reply
            .to_bytes(Some(password.as_bytes()), &mut buf, sha1_hmac)
            .expect("IO error writing STUN error reply");
        buf.truncate(n);

        let trans = Transmit {
            proto: req.proto,
            source: req.destination,
            destination: req.source,
            contents: buf.into(),
        };

        self.transmit.push_back(trans);
    }

    /// Switches the controlling/controlled role and recomputes pair priorities
    /// per RFC 8445 §7.3.1.1.
    fn switch_role(&mut self, controlling: bool) {
        if self.controlling == controlling {
            return;
        }

        debug!(
            "Switch role: {} -> {}",
            role_name(self.controlling),
            role_name(controlling),
        );

        self.controlling = controlling;

        for i in 0..self.candidate_pairs.len() {
            let local_idx = self.candidate_pairs[i].local_idx();
            let remote_idx = self.candidate_pairs[i].remote_idx();
            let local_prio = self.local_candidates[local_idx].prio();
            let remote_prio = self.remote_candidates[remote_idx].prio();
            let new_prio = CandidatePair::calculate_prio(controlling, remote_prio, local_prio);
            self.candidate_pairs[i].set_prio(new_prio);
        }
        self.candidate_pairs.sort();
    }

    fn stun_client_binding_request(&mut self, now: Instant, pair_idx: usize) {
        let (username, password) = self.stun_credentials(false);

        let pair = &mut self.candidate_pairs[pair_idx];
        let local = pair.local_candidate(&self.local_candidates);
        let remote = pair.remote_candidate(&self.remote_candidates);
        let prio = local.prio_prflx();
        // Only the controlling side sends USE-CANDIDATE.
        let use_candidate = self.controlling && pair.is_nominated();

        let trans_id = pair.new_attempt(now, &self.timing_config, self.controlling);

        self.stats.bind_request_sent += 1;

        let binding = StunMessage::binding_request(
            &username,
            trans_id,
            self.controlling,
            self.control_tie_breaker,
            prio,
            use_candidate,
        );

        trace!(
            "Send STUN request: {} -> {} {:?}",
            local.base(),
            remote.addr(),
            binding
        );

        let mut buf = vec![0_u8; self.mtu()];

        let sha1_hmac =
            |key: &[u8], payloads: &[&[u8]]| self.sha1_hmac_provider.sha1_hmac(key, payloads);
        let n = binding
            .to_bytes(Some(password.as_bytes()), &mut buf, sha1_hmac)
            .expect("IO error writing STUN reply");
        buf.truncate(n);

        let trans = Transmit {
            proto: local.proto(),
            source: local.base(),
            destination: remote.addr(),
            contents: buf.into(),
        };

        self.transmit.push_back(trans);
    }

    /// Handle a STUN binding error response.
    ///
    /// We only act on 487 (Role Conflict) per RFC 8445 §7.3.1.1: switch to
    /// the opposite role, but only if the request that triggered the 487 was
    /// sent under our current role. Otherwise the response is stale —
    /// in-flight from before an earlier swap — and acting on it would flap.
    fn stun_client_handle_error(&mut self, message: StunMessage<'_>) {
        let Some((code, _)) = message.error_code() else {
            return;
        };
        if code != 487 {
            debug!("Ignoring binding error response with code {code}");
            return;
        }

        let trans_id = message.trans_id();
        let attempt = self
            .candidate_pairs
            .iter()
            .find_map(|p| p.binding_attempt(trans_id));

        let Some(attempt) = attempt else {
            debug!("Ignore 487 for unknown transaction id");
            return;
        };

        if attempt.controlling() != self.controlling {
            debug!(
                "Ignore stale 487 (request was sent as {}, we are now {})",
                role_name(attempt.controlling()),
                role_name(self.controlling),
            );
            return;
        }

        debug!(
            "Got 487 Role Conflict, switching from {}",
            role_name(self.controlling),
        );
        self.switch_role(!self.controlling);
    }

    fn stun_client_handle_response(&mut self, now: Instant, message: StunMessage<'_>) {
        // Find the candidate pair that this trans_id was sent for.
        let trans_id = message.trans_id();
        let maybe_pair = self
            .candidate_pairs
            .iter_mut()
            .find(|p| p.binding_attempt(trans_id).is_some());

        self.stats.bind_success_recv += 1;

        let pair = match maybe_pair {
            Some(v) => v,
            // Not finding the candidate pair is fine. That might mean the
            // binding response came in "too late", after we discarded the
            // pair for some reason.
            None => {
                debug!("No pair found for STUN response: {:?}", message);
                return;
            }
        };

        // The ICE agent MUST check the mapped address from the STUN response.
        // If the transport address does not match any of the local candidates
        // that the agent knows about, the mapped address represents a new
        // candidate: a peer-reflexive candidate.
        let mapped_address = message
            .mapped_address()
            // This should be caught in the parsing.
            .expect("Mapped address in STUN response");

        let found_in_local = self
            .local_candidates
            .iter()
            .enumerate()
            .find(|(_, c)| c.addr() == mapped_address);

        let (pair, valid_idx) = if let Some((valid_idx, _)) = found_in_local {
            // Note, the valid_idx might not be the same as the local_idx that we
            // sent the request from. This might happen for hosts with asymmetric
            // routing, traffic leaving on one interface and responses coming back
            // on another.
            trace!(
                "Found local candidate for mapped address: {}",
                mapped_address
            );
            (pair, valid_idx)
        } else {
            let local_sent_from = pair.local_candidate(&self.local_candidates);

            // Like other candidates, a peer-reflexive candidate has a type, base, priority,
            // and foundation. They are computed as follows:

            // o  The priority is the value of the PRIORITY attribute in the Binding
            //     request.
            //
            // The PRIORITY attribute MUST be included in a Binding request and be
            // set to the value computed by the algorithm in Section 5.1.2 for the
            // local candidate, but with the candidate type preference of peer-
            // reflexive candidates.
            let prio = local_sent_from.prio_prflx();

            // o  The base is the local candidate of the candidate pair from which
            //     the Binding request was sent.
            let base = local_sent_from.base();

            // o  The type is peer reflexive.
            let candidate = Candidate::peer_reflexive(
                local_sent_from.proto(),
                mapped_address,
                base,
                prio,
                None,
                self.local_credentials.ufrag.clone(),
            );

            debug!(
                "Created local peer reflexive candidate for mapped address: {}",
                Pii(&mapped_address)
            );

            // The ICE agent does not need to pair the peer-reflexive candidate with
            // remote candidates.
            // If an agent wishes to pair the peer-reflexive candidate with remote
            // candidates other than the one in the valid pair that will be generated,
            // the agent MAY provide updated candidate information to the peer that includes
            // the peer-reflexive candidate.  This will cause the peer-reflexive candidate
            // to be paired with all other remote candidates.

            // For now we do not tell the other side about discovered peer-reflexive candidates.
            // We just include it in our list of local candidates and use it for the "valid pair".
            self.local_candidates.push(candidate);

            let idx = self.local_candidates.len() - 1;

            (pair, idx)
        };

        pair.record_binding_response(now, trans_id, valid_idx);

        // For a controlled agent, a successful check can make a nominated pair valid.
        self.evaluate_nomination();

        // State might change when we get a response.
        self.evaluate_state(now);
    }

    fn evaluate_nomination(&mut self) {
        let nominated_pair_priority = self.nominated_pair_priority();

        let best_prio = if self.controlling {
            // For controlling agents, we pick the best candidate pair using
            // this strategy.
            self.candidate_pairs
                .iter_mut()
                .filter(|p| p.state() == CheckState::Succeeded)
                .max_by_key(|p| p.prio())
        } else {
            // For controlled agents, we pick the best pair from what the controlling
            // agent has indicated with USE-CANDIDATE stun attribute. RFC 8445 §7.3.1.5
            // lets us act on a nomination only once the pair is valid, i.e. our own
            // check on it succeeded, so a pair the peer nominated but we cannot reach
            // never becomes the send path.
            self.candidate_pairs
                .iter_mut()
                .filter(|p| p.is_nominated() && p.state() == CheckState::Succeeded)
                .max_by_key(|p| p.prio())
        };

        if let Some(best_prio) = best_prio {
            if let Some(nominated) = nominated_pair_priority {
                if nominated == best_prio.prio() {
                    // The best prio is also the current nominated prio. Make
                    // no changes since there can be multiple pairs having the
                    // same best_prio.
                    return;
                }
            }
            trace!("Nominating best candidate");

            if !best_prio.is_nominated() && (self.controlling || self.ice_lite) {
                // ice lite progresses pair to success straight away.
                best_prio.nominate(self.ice_lite);
            }

            let local = best_prio.local_candidate(&self.local_candidates);
            let remote = best_prio.remote_candidate(&self.remote_candidates);

            self.nominated_send = Some(best_prio.id());
            self.emit_event(IceAgentEvent::NominatedSend {
                proto: local.proto(),
                source: local.base(),
                destination: remote.addr(),
            })
        }
    }

    fn nominated_pair_priority(&self) -> Option<u64> {
        let id = self.nominated_send?;

        self.candidate_pairs
            .iter()
            .find_map(|p| (p.id() == id).then_some(p.prio()))
    }

    fn nominated_pair(&self) -> Option<&CandidatePair> {
        let id = self.nominated_send?;
        self.candidate_pairs.iter().find(|p| p.id() == id)
    }

    /// The round-trip time of the most recent successful STUN binding
    /// transaction on the nominated candidate pair.
    ///
    /// Returns `None` if there is no nominated pair, or no successful
    /// binding response has been recorded on it.
    ///
    /// This is the ICE-level equivalent of
    /// [`RTCIceCandidatePairStats.currentRoundTripTime`][1] and is updated
    /// on every STUN keepalive/consent exchange — useful for measuring RTT
    /// on receive-only endpoints where RTP-based RTT estimates aren't
    /// available.
    ///
    /// [1]: https://www.w3.org/TR/webrtc-stats/#dom-rtcicecandidatepairstats-currentroundtriptime
    pub fn nominated_pair_rtt(&self) -> Option<Duration> {
        self.nominated_pair().and_then(|p| p.last_successful_rtt())
    }

    /// Sum of all RTTs measured from successful STUN binding transactions
    /// on the nominated candidate pair over its lifetime.
    ///
    /// Spec equivalent of [`RTCIceCandidatePairStats.totalRoundTripTime`][1].
    /// Divide by [`nominated_pair_responses_received`] to get the average.
    ///
    /// Returns `None` if there is no nominated pair.
    ///
    /// [`nominated_pair_responses_received`]: Self::nominated_pair_responses_received
    /// [1]: https://www.w3.org/TR/webrtc-stats/#dom-rtcicecandidatepairstats-totalroundtriptime
    pub fn nominated_pair_total_rtt(&self) -> Option<Duration> {
        self.nominated_pair().map(|p| p.total_round_trip_time())
    }

    /// Total number of STUN binding responses received on the nominated
    /// candidate pair over its lifetime.
    ///
    /// Spec equivalent of [`RTCIceCandidatePairStats.responsesReceived`][1].
    ///
    /// Returns `None` if there is no nominated pair.
    ///
    /// [1]: https://www.w3.org/TR/webrtc-stats/#dom-rtcicecandidatepairstats-responsesreceived
    pub fn nominated_pair_responses_received(&self) -> Option<u64> {
        self.nominated_pair().map(|p| p.responses_received())
    }

    fn set_connection_state(&mut self, state: IceConnectionState, reason: &'static str) {
        if self.state != state {
            debug!("State change ({}): {:?} -> {:?}", reason, self.state, state);
            self.state = state;
            self.emit_event(IceAgentEvent::IceConnectionStateChange(state));
        }
    }

    fn evaluate_state(&mut self, now: Instant) {
        use IceConnectionState::*;

        let mut any_nomination = false;
        let mut any_still_possible = false;

        for p in &self.candidate_pairs {
            // A nominated pair counts once it is valid (RFC 8445 §8.1.1), which is
            // also when `evaluate_nomination` can make it the send path. Until then
            // it is just another pair being checked.
            if p.is_nominated() && p.state() == CheckState::Succeeded {
                any_nomination = true;
            } else if p.is_still_possible(now, &self.timing_config) {
                any_still_possible = true;
            }
        }

        // As a special case, before the ice agent has received any candidates or
        // discovered a peer reflexive via a STUN message, the agent is still viable. This is
        // also the case for ice_restart.
        if self.remote_candidates.is_empty() || self.local_candidates.is_empty() {
            any_still_possible = true;
        }

        match self.state {
            New => {
                self.set_connection_state(Checking, "new connection");
            }
            Checking | Disconnected => {
                if any_nomination {
                    if self.ice_lite {
                        self.set_connection_state(Completed, "got nomination in ice lite");
                        return;
                    }
                    if any_still_possible {
                        self.set_connection_state(Connected, "got nomination, still trying others");
                    } else {
                        self.set_connection_state(Completed, "got nomination, no others to try");
                    }
                } else if !any_still_possible {
                    self.set_connection_state(Disconnected, "no possible pairs");
                } else {
                    self.set_connection_state(Checking, "got new possible");
                }
            }
            Connected => {
                if any_nomination {
                    if !any_still_possible {
                        self.set_connection_state(Completed, "no more possible to try");
                    }
                } else if any_still_possible {
                    self.set_connection_state(Checking, "got new possible");
                } else {
                    self.set_connection_state(Disconnected, "none nominated");
                }
            }
            Completed => {
                if any_nomination {
                    if any_still_possible && !self.ice_lite {
                        self.set_connection_state(Connected, "got new possible");
                    }
                } else if any_still_possible {
                    self.set_connection_state(Checking, "got new possible");
                } else {
                    self.set_connection_state(Disconnected, "none nominated");
                }
            }
        }
    }
}

impl fmt::Debug for LocalPreferenceHolder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("LocalPreferenceHolder").finish()
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::DefaultSha1HmacProvider;
    use std::{iter, net::SocketAddr};

    fn host(s: impl Into<String>, proto: impl TryInto<Protocol>) -> Candidate {
        let s: String = s.into();
        Candidate::host(s.parse().unwrap(), proto).unwrap()
    }

    impl IceAgent {
        pub(crate) fn num_candidate_pairs(&self) -> usize {
            self.candidate_pairs.len()
        }

        fn pair_indexes(&self) -> Vec<(usize, usize)> {
            self.candidate_pairs
                .iter()
                .map(|c| (c.local_idx(), c.remote_idx()))
                .collect()
        }
    }

    fn new_test_agent() -> IceAgent {
        IceAgent::new(IceCreds::new())
    }

    fn ipv4_1() -> SocketAddr {
        "1.2.3.4:5000".parse().unwrap()
    }
    fn ipv4_2() -> SocketAddr {
        "2.3.4.5:5000".parse().unwrap()
    }
    fn ipv4_3() -> SocketAddr {
        "3.4.5.6:5000".parse().unwrap()
    }
    fn ipv4_4() -> SocketAddr {
        "4.5.6.7:5000".parse().unwrap()
    }
    fn ipv6_1() -> SocketAddr {
        "[1001::]:5000".parse().unwrap()
    }
    fn ipv6_2() -> SocketAddr {
        "[1002::]:5000".parse().unwrap()
    }

    #[test]
    fn local_preference_host() {
        let mut agent = new_test_agent();

        agent
            .add_local_candidate(Candidate::host(ipv4_1(), "udp").unwrap())
            .unwrap();
        agent
            .add_local_candidate(Candidate::host(ipv6_1(), "udp").unwrap())
            .unwrap();
        agent
            .add_local_candidate(Candidate::host(ipv6_2(), "udp").unwrap())
            .unwrap();
        agent
            .add_local_candidate(Candidate::host(ipv4_2(), "udp").unwrap())
            .unwrap();

        let v: Vec<_> = agent
            .local_candidates
            .iter()
            .map(|c| c.local_preference())
            .collect();

        assert_eq!(v, vec![65534, 65535, 65533, 65532]);
    }

    #[test]
    fn discard_adding_redundant() {
        let mut agent = new_test_agent();

        // Frequently, a server-reflexive candidate and a host candidate will be
        // redundant when the agent is not behind a NAT.

        let x2 = agent.add_local_candidate(Candidate::host(ipv4_1(), "udp").unwrap());
        assert!(x2.is_some());

        // this is redundant given we have the direct host candidate above.
        let x1 = agent.add_local_candidate(Candidate::test_peer_rflx(ipv4_1(), ipv4_1(), "udp"));
        assert!(x1.is_none());
    }

    #[test]
    fn does_not_invalidate_local_candidate_with_same_ip_but_different_kind() {
        let mut agent = new_test_agent();
        let host = Candidate::host(ipv4_1(), "udp").unwrap();
        let srflx = Candidate::server_reflexive(ipv4_1(), ipv4_1(), "udp").unwrap();

        agent.add_local_candidate(host.clone()).unwrap();
        let invalidated = agent.invalidate_candidate(&srflx);
        assert!(!invalidated);

        let invalidated = agent.invalidate_candidate(&host);
        assert!(invalidated);
    }

    #[test]
    fn does_not_invalidate_local_candidate_with_same_ip_but_different_protocol() {
        let mut agent = new_test_agent();
        let udp = Candidate::host(ipv4_1(), "udp").unwrap();
        let tcp = Candidate::host(ipv4_1(), "tcp").unwrap();

        agent.add_local_candidate(udp.clone()).unwrap();
        agent.add_local_candidate(tcp.clone()).unwrap();

        let invalidated = agent.invalidate_candidate(&tcp);
        assert!(invalidated);

        let udp_discarded = agent
            .local_candidates
            .iter()
            .find(|v| {
                v.addr() == udp.addr()
                    && v.base() == udp.base()
                    && v.raddr() == udp.raddr()
                    && v.kind() == udp.kind()
                    && v.proto() == udp.proto()
            })
            .unwrap()
            .discarded();
        assert!(!udp_discarded);

        let tcp_discarded = agent
            .local_candidates
            .iter()
            .find(|v| {
                v.addr() == tcp.addr()
                    && v.base() == tcp.base()
                    && v.raddr() == tcp.raddr()
                    && v.kind() == tcp.kind()
                    && v.proto() == tcp.proto()
            })
            .unwrap()
            .discarded();
        assert!(tcp_discarded);

        let invalidated = agent.invalidate_candidate(&udp);
        assert!(invalidated);
        let udp_discarded = agent
            .local_candidates
            .iter()
            .find(|v| {
                v.addr() == udp.addr()
                    && v.base() == udp.base()
                    && v.raddr() == udp.raddr()
                    && v.kind() == udp.kind()
                    && v.proto() == udp.proto()
            })
            .unwrap()
            .discarded();
        assert!(udp_discarded);
    }

    #[test]
    fn does_not_invalidate_remote_candidate_with_same_ip_but_different_kind() {
        let mut agent = new_test_agent();
        let host = Candidate::host(ipv4_1(), "udp").unwrap();
        let srflx = Candidate::server_reflexive(ipv4_1(), ipv4_1(), "udp").unwrap();

        agent.add_remote_candidate(host.clone());
        let invalidated = agent.invalidate_candidate(&srflx);

        assert!(!invalidated);

        let invalidated = agent.invalidate_candidate(&host);
        assert!(invalidated);
    }

    #[test]
    fn does_not_invalidate_remote_candidate_with_same_ip_but_different_protocol() {
        let mut agent = new_test_agent();
        let udp = Candidate::host(ipv4_1(), "udp").unwrap();
        let tcp = Candidate::host(ipv4_1(), "tcp").unwrap();

        agent.add_remote_candidate(udp.clone());
        agent.add_remote_candidate(tcp.clone());

        let invalidated = agent.invalidate_candidate(&tcp);
        assert!(invalidated);

        let udp_discarded = agent
            .remote_candidates
            .iter()
            .find(|v| {
                v.addr() == udp.addr()
                    && v.base() == udp.base()
                    && v.raddr() == udp.raddr()
                    && v.kind() == udp.kind()
                    && v.proto() == udp.proto()
            })
            .unwrap()
            .discarded();
        assert!(!udp_discarded);

        let tcp_discarded = agent
            .remote_candidates
            .iter()
            .find(|v| {
                v.addr() == tcp.addr()
                    && v.base() == tcp.base()
                    && v.raddr() == tcp.raddr()
                    && v.kind() == tcp.kind()
                    && v.proto() == tcp.proto()
            })
            .unwrap()
            .discarded();
        assert!(tcp_discarded);

        let invalidated = agent.invalidate_candidate(&udp);
        assert!(invalidated);
        let udp_discarded = agent
            .remote_candidates
            .iter()
            .find(|v| {
                v.addr() == udp.addr()
                    && v.base() == udp.base()
                    && v.raddr() == udp.raddr()
                    && v.kind() == udp.kind()
                    && v.proto() == udp.proto()
            })
            .unwrap()
            .discarded();
        assert!(udp_discarded);
    }

    #[test]
    fn discard_adding_redundant_by_address_and_protocol() {
        let mut agent = new_test_agent();

        // Candidates with the same SocketAddr but different protocols are considered distinct.
        assert!(
            agent
                .add_local_candidate(Candidate::host(ipv4_1(), "udp").unwrap())
                .is_some()
        );
        assert!(
            agent
                .add_local_candidate(Candidate::host(ipv4_1(), "tcp").unwrap())
                .is_some()
        );
        assert!(
            agent
                .add_local_candidate(Candidate::host(ipv4_1(), "ssltcp").unwrap())
                .is_some()
        );

        // Verify these are rejected, since these tuples of address and protocol have been added.
        assert!(
            agent
                .add_local_candidate(Candidate::host(ipv4_1(), "udp").unwrap())
                .is_none()
        );
        assert!(
            agent
                .add_local_candidate(Candidate::host(ipv4_1(), "ssltcp").unwrap())
                .is_none()
        );

        // Verify these are allowed, since these have different addresses.
        assert!(
            agent
                .add_local_candidate(Candidate::host(ipv4_2(), "udp").unwrap())
                .is_some()
        );
        assert!(
            agent
                .add_local_candidate(Candidate::host(ipv4_2(), "ssltcp").unwrap())
                .is_some()
        );
    }

    #[test]
    fn discard_already_added_redundant() {
        let mut agent = new_test_agent();

        // Frequently, a server-reflexive candidate and a host candidate will be
        // redundant when the agent is not behind a NAT.

        // this is contrived, but it is redundant when we add the host candidate below.
        let x1 = agent.add_local_candidate(Candidate::test_peer_rflx(ipv4_1(), ipv4_1(), "udp"));
        assert!(x1.is_some());

        let x2 = agent.add_local_candidate(Candidate::host(ipv4_1(), "udp").unwrap());
        assert!(x2.is_some());

        let v: Vec<_> = agent
            .local_candidates
            .iter()
            .map(|v| v.discarded())
            .collect();

        assert_eq!(v, vec![true, false]);
    }

    #[test]
    fn form_pairs() {
        let mut agent = new_test_agent();

        // local 0
        agent
            .add_local_candidate(Candidate::host(ipv4_1(), "udp").unwrap())
            .unwrap();
        // local 1 "udp"
        agent
            .add_local_candidate(Candidate::test_peer_rflx(ipv4_4(), ipv4_2(), "udp"))
            .unwrap();
        // local 2 "tcp"
        agent
            .add_local_candidate(Candidate::host(ipv4_1(), "tcp").unwrap())
            .unwrap();

        // remote 0
        agent.add_remote_candidate(Candidate::test_peer_rflx(ipv4_4(), ipv4_3(), "udp"));
        // remote 1 "udp"
        agent.add_remote_candidate(Candidate::host(ipv4_3(), "udp").unwrap());
        // remote 2 "tcp"
        agent.add_remote_candidate(Candidate::host(ipv4_3(), "tcp").unwrap());

        // we expect:
        // (host/udp host/udp) - (0, 1)
        // (host/udp rflx/udp) - (0, 1)
        // (rflx/udp host/udp) - (1, 1)
        // (rflx/udp rflx/udp) - (1, 0)
        // (host/tcp host/tcp) - (2, 2)

        assert_eq!(
            agent.pair_indexes(),
            [(0, 1), (0, 0), (1, 1), (1, 0), (2, 2)]
        );
    }

    #[test]
    fn form_pairs_skip_redundant() {
        let mut agent = new_test_agent();

        agent.add_remote_candidate(Candidate::host(ipv4_3(), "udp").unwrap());
        agent.add_remote_candidate(Candidate::host(ipv4_3(), "tcp").unwrap());
        agent
            .add_local_candidate(Candidate::host(ipv4_1(), "udp").unwrap())
            .unwrap();

        // the UDP candidates should be pair up.
        assert_eq!(agent.pair_indexes(), [(0, 0)]);

        // this local UDP candidate is redundant an won't form a new pair.
        agent
            .add_local_candidate(Candidate::test_peer_rflx(ipv4_2(), ipv4_1(), "udp"))
            .unwrap();

        assert_eq!(agent.pair_indexes(), [(0, 0)]);

        // this local TCP candidate will be paired up (This is the 3rd local candidate)
        agent
            .add_local_candidate(Candidate::test_peer_rflx(ipv4_2(), ipv4_1(), "tcp"))
            .unwrap();

        assert_eq!(agent.pair_indexes(), [(0, 0), (2, 1)]);
    }

    #[test]
    fn form_pairs_replace_redundant() {
        let mut agent = new_test_agent();

        agent.add_remote_candidate(Candidate::host(ipv4_3(), "udp").unwrap());
        agent
            .add_local_candidate(Candidate::test_peer_rflx(ipv4_2(), ipv4_1(), "udp"))
            .unwrap();

        assert_eq!(agent.pair_indexes(), [(0, 0)]);

        // this local candidate is redundant, but has higher priority than then existing pair.
        // it replaces the existing pair.
        agent
            .add_local_candidate(Candidate::host(ipv4_1(), "udp").unwrap())
            .unwrap();

        assert_eq!(agent.pair_indexes(), [(1, 0)]);
    }

    #[test]
    fn form_pairs_replace_remote_redundant() {
        let mut agent = new_test_agent();
        agent.set_ice_lite(true);

        // This is just prepping the test, this would have been discovered in a STUN packet.
        let c = Candidate::peer_reflexive(
            "udp",
            ipv4_3(),
            ipv4_3(),
            123,
            Some(REMOTE_PEER_REFLEXIVE_TEMP_FOUNDATION.into()),
            "".to_string(),
        );

        agent.add_remote_candidate(c);
        agent
            .add_local_candidate(Candidate::host(ipv4_1(), "udp").unwrap())
            .unwrap();

        assert_eq!(agent.pair_indexes(), [(0, 0)]);

        let now = Instant::now();
        agent.candidate_pairs[0].nominate(true);
        agent.candidate_pairs[0].increase_remote_binding_requests(now);

        // this remote should replace the "discovered" peer reflexive added above.
        agent.add_remote_candidate(Candidate::host(ipv4_3(), "udp").unwrap());

        // The index should not have changed, since we replaced the peer reflexive remote candidate.
        assert_eq!(agent.pair_indexes(), [(0, 0)]);

        let pair = &agent.candidate_pairs[0];
        assert!(pair.is_nominated());

        let (req, time) = pair.remote_binding_requests();
        assert_eq!(req, 1);
        assert_eq!(time, Some(now));
    }

    #[test]
    fn form_pairs_skip_invalidated_local() {
        let mut agent = new_test_agent();

        let local = Candidate::test_peer_rflx(ipv4_2(), ipv4_1(), "udp");

        agent.add_local_candidate(local.clone()).unwrap();
        agent.invalidate_candidate(&local);

        agent.add_remote_candidate(Candidate::host(ipv4_3(), "udp").unwrap());

        // There should be no pairs since we invalidated the local candidate.
        assert_eq!(agent.pair_indexes(), []);
    }

    #[test]
    fn form_pairs_skip_invalidated_remote() {
        let mut agent = new_test_agent();

        let remote = Candidate::host(ipv4_3(), "udp").unwrap();

        agent.add_remote_candidate(remote.clone());
        agent.invalidate_candidate(&remote);

        agent
            .add_local_candidate(Candidate::test_peer_rflx(ipv4_2(), ipv4_1(), "udp"))
            .unwrap();

        // There should be no pairs since we invalidated the local candidate.
        assert_eq!(agent.pair_indexes(), []);
    }

    #[test]
    fn poll_time_must_timing_advance() {
        let mut agent = new_test_agent();
        agent
            .add_local_candidate(Candidate::host(ipv4_1(), "udp").unwrap())
            .unwrap();
        agent.add_remote_candidate(Candidate::host(ipv4_3(), "udp").unwrap());

        let now1 = Instant::now();
        agent.handle_timeout(now1);
        let now2 = agent.poll_timeout().unwrap();

        assert!(now2 - now1 == Duration::from_millis(50));
    }

    #[test]
    fn no_disconnect_before_remote_candidates() {
        let mut agent = new_test_agent();

        let now = Instant::now();
        agent.handle_timeout(now);

        while let Some(ev) = agent.poll_event() {
            if let IceAgentEvent::IceConnectionStateChange(s) = ev {
                assert!(s != IceConnectionState::Disconnected);
            }
        }

        agent.handle_timeout(now + Duration::from_millis(200));
        while let Some(ev) = agent.poll_event() {
            if let IceAgentEvent::IceConnectionStateChange(s) = ev {
                assert!(s != IceConnectionState::Disconnected);
            }
        }
    }

    #[test]
    fn does_not_accept_response_with_unknown_transaction_id() {
        let mut agent = new_test_agent();
        let remote_creds = IceCreds::new();
        let mut remote_candidate = Candidate::host(ipv4_3(), "udp").unwrap();
        remote_candidate.set_ufrag(&remote_creds.ufrag);

        agent.set_remote_credentials(remote_creds.clone());
        agent
            .add_local_candidate(Candidate::host(ipv4_1(), "udp").unwrap())
            .unwrap();
        agent.add_remote_candidate(remote_candidate);
        agent.handle_timeout(Instant::now());

        let payload = Vec::from(agent.poll_transmit().unwrap().contents);
        let stun_message = StunMessage::parse(&payload).unwrap();

        let valid_reply =
            make_authenticated_stun_reply(stun_message.trans_id(), ipv4_4(), &remote_creds.pass);
        let fake_reply =
            make_authenticated_stun_reply(TransId::new(), ipv4_4(), &remote_creds.pass);

        assert!(!agent.accepts_message(&StunMessage::parse(&fake_reply).unwrap()));
        assert!(agent.accepts_message(&StunMessage::parse(&valid_reply).unwrap()));
    }

    #[test]
    fn ignore_binding_indication() {
        // STUN Binding Indication from OBS WHIP Client using Wireshark
        //
        // Session Traversal Utilities for NAT
        // Message Type: 0x0011 (Binding Indication)
        // Message Length: 8
        // Message Cookie: 2112a442
        // Message Transaction ID: fb9859e67da4bc991c0cab8f
        // [STUN Network Version: RFC-5389/8489 (3)]
        // Attributes
        //     FINGERPRINT
        //         Attribute Type: FINGERPRINT
        //         Attribute Length: 4
        //         CRC-32: 0xed80c297 [correct]
        //         [CRC-32 Status: Good]
        const BINDING: &[u8] = &[
            0x00, 0x11, 0x00, 0x08, 0x21, 0x12, 0xa4, 0x42, 0xfb, 0x98, 0x59, 0xe6, 0x7d, 0xa4,
            0xbc, 0x99, 0x1c, 0x0c, 0xab, 0x8f, 0x80, 0x28, 0x00, 0x04, 0xed, 0x80, 0xc2, 0x97,
        ];
        let stun_msg = StunMessage::parse(BINDING).unwrap();

        let agent = new_test_agent();
        assert!(!agent.accepts_message(&stun_msg));
    }

    #[test]
    fn queues_stun_binding_before_remote_creds() {
        let mut agent = new_test_agent();
        agent
            .add_local_candidate(Candidate::host(ipv4_1(), "udp").unwrap())
            .unwrap();

        let remote_creds = IceCreds::new();
        let mut remote_candidate = Candidate::host(ipv4_3(), "udp").unwrap();
        remote_candidate.set_ufrag(&remote_creds.ufrag);
        let prio = remote_candidate.prio();
        agent.add_remote_candidate(remote_candidate);

        let serialized_req = make_serialized_binding_request(
            &agent.local_credentials,
            &remote_creds,
            !agent.controlling(),
            prio,
        );
        let binding_req = StunMessage::parse(&serialized_req).unwrap();

        // Should not be dropped
        assert!(agent.accepts_message(&binding_req));
        agent.handle_packet(
            Instant::now(),
            StunPacket {
                message: binding_req,
                source: ipv4_3(),
                destination: ipv4_1(),
                proto: Protocol::Udp,
            },
        );

        // Should not yet get a response
        agent.handle_timeout(Instant::now());
        assert!(agent.poll_transmit().is_none());

        agent.set_remote_credentials(remote_creds.clone());
        agent.handle_timeout(Instant::now());

        // Now should have a response
        let payload = Vec::from(agent.poll_transmit().unwrap().contents);
        let stun_message = StunMessage::parse(&payload).unwrap();
        assert!(stun_message.is_successful_binding_response());
    }

    /// On simultaneous controlling/controlling startup, the lower-tiebreaker
    /// agent typically receives the peer's request first (server-side swap)
    /// while its own in-flight requests — sent under the old role — are
    /// still being 487'd by the peer. Each of those stale 487s must be
    /// ignored, otherwise the role flaps with every retransmit until the
    /// in-flight drain finishes.
    #[test]
    pub fn ignores_stale_487_after_role_swap() {
        let mut agent = new_test_agent();
        agent.set_controlling(true);
        let remote_creds = IceCreds::new();
        agent.set_remote_credentials(remote_creds.clone());
        agent
            .add_local_candidate(Candidate::host(ipv4_1(), "udp").unwrap())
            .unwrap();
        let mut remote_candidate = Candidate::host(ipv4_3(), "udp").unwrap();
        remote_candidate.set_ufrag(&remote_creds.ufrag);
        agent.add_remote_candidate(remote_candidate);

        // Drive one binding request out — recorded as `controlling=true`.
        agent.handle_timeout(Instant::now());
        let req_payload = Vec::from(agent.poll_transmit().unwrap().contents);
        let trans_id = StunMessage::parse(&req_payload).unwrap().trans_id();

        // Simulate that an earlier 487 (or a server-side conflict from an
        // incoming peer request) already moved us to controlled.
        agent.set_controlling(false);

        // Now the original request's 487 lands. It is stale: the attempt was
        // sent as controlling, but we are no longer controlling.
        let reply = make_role_conflict_reply(trans_id, &remote_creds.pass);
        agent.handle_packet(
            Instant::now(),
            StunPacket {
                proto: Protocol::Udp,
                source: ipv4_3(),
                destination: ipv4_1(),
                message: StunMessage::parse(&reply).unwrap(),
            },
        );

        assert!(
            !agent.controlling(),
            "stale 487 must not flip role back to controlling"
        );
    }

    #[test]
    pub fn discards_packet_from_unknown_candidate() {
        let mut agent = new_test_agent();
        let remote_creds = IceCreds::new();
        agent.set_remote_credentials(remote_creds.clone());

        let request =
            make_serialized_binding_request(&agent.local_credentials, &remote_creds, false, 0);

        agent.handle_packet(
            Instant::now(),
            StunPacket {
                proto: Protocol::Udp,
                source: ipv4_1(),
                destination: ipv4_2(),
                message: StunMessage::parse(&request).unwrap(),
            },
        );

        assert!(agent.poll_transmit().is_none());
    }

    #[test]
    pub fn no_disconnect_missing_local_candidates() {
        let mut agent = new_test_agent();
        agent.set_remote_credentials(IceCreds::new().clone());

        agent.add_remote_candidate(host("1.1.1.1:1000", "udp"));
        agent.handle_timeout(Instant::now());
        agent.handle_timeout(Instant::now() + Duration::from_millis(100));

        let events = iter::from_fn(|| agent.poll_event()).collect::<Vec<_>>();
        assert!(!events.contains(&IceAgentEvent::IceConnectionStateChange(
            IceConnectionState::Disconnected
        )));
    }

    #[test]
    fn discarded_local_candidates_are_not_returned() {
        let mut agent = new_test_agent();
        let host1 = Candidate::host(ipv4_1(), "udp").unwrap();
        let host2 = Candidate::host(ipv4_2(), "udp").unwrap();

        let host1 = agent.add_local_candidate(host1.clone()).unwrap().clone();
        let host2 = agent.add_local_candidate(host2.clone()).unwrap().clone();
        agent.invalidate_candidate(&host1);

        assert_eq!(agent.local_candidates().collect::<Vec<_>>(), vec![host2]);
    }

    #[test]
    fn discarded_remote_candidates_are_not_returned() {
        let mut agent = new_test_agent();
        let host1 = Candidate::host(ipv4_1(), "udp").unwrap();
        let host2 = Candidate::host(ipv4_2(), "udp").unwrap();

        agent.add_remote_candidate(host1.clone());
        agent.add_remote_candidate(host2.clone());
        agent.invalidate_candidate(&host1);

        assert_eq!(agent.remote_candidates().collect::<Vec<_>>(), vec![host2]);
    }

    #[test]
    fn ice_lite_remote_candidate_viable_after_binding_request() {
        let mut agent = new_test_agent();
        agent.set_ice_lite(true);
        agent.set_controlling(false);
        agent
            .add_local_candidate(Candidate::host(ipv4_1(), "udp").unwrap())
            .unwrap();

        let remote_creds = IceCreds::new();
        agent.set_remote_credentials(remote_creds.clone());

        let mut remote_candidate = Candidate::host(ipv4_3(), "udp").unwrap();
        remote_candidate.set_ufrag(&remote_creds.ufrag);
        let prio = remote_candidate.prio();
        agent.add_remote_candidate(remote_candidate);

        // Nothing has been heard from the remote yet.
        assert!(!agent.has_viable_remote_candidate(ipv4_3()));

        let serialized_req = make_serialized_binding_request(
            &agent.local_credentials,
            &remote_creds,
            true, // the remote is the controlling agent
            prio,
        );
        let binding_req = StunMessage::parse(&serialized_req).unwrap();

        let now = Instant::now();
        assert!(agent.handle_packet(
            now,
            StunPacket {
                message: binding_req,
                source: ipv4_3(),
                destination: ipv4_1(),
                proto: Protocol::Udp,
            },
        ));

        // We answered a binding request whose integrity checked out against our
        // local password. An ice-lite agent sends no checks of its own, so this
        // is the only validation the pair will ever get, and it must be enough
        // for the remote address to count as viable.
        assert!(agent.has_viable_remote_candidate(ipv4_3()));
    }

    #[test]
    fn ice_lite_validation_survives_trickle() {
        let mut agent = new_test_agent();
        agent.set_ice_lite(true);
        agent.set_controlling(false);
        agent
            .add_local_candidate(Candidate::host(ipv4_1(), "udp").unwrap())
            .unwrap();
        let remote_creds = IceCreds::new();
        agent.set_remote_credentials(remote_creds.clone());
        // Receive a check before the remote host candidate is signaled.
        let remote = Candidate::host(ipv4_3(), "udp").unwrap();
        let serialized = make_serialized_binding_request(
            &agent.local_credentials,
            &remote_creds,
            true,
            remote.prio() - 1,
        );
        assert!(agent.handle_packet(
            Instant::now(),
            StunPacket {
                message: StunMessage::parse(&serialized).unwrap(),
                source: ipv4_3(),
                destination: ipv4_1(),
                proto: Protocol::Udp,
            }
        ));
        assert!(agent.has_viable_remote_candidate(ipv4_3()));
        // The higher-priority signaled candidate replaces the peer-reflexive pair.
        agent.add_remote_candidate(remote);
        assert!(
            agent.has_viable_remote_candidate(ipv4_3()),
            "trickling the real candidate must retain validated ingress"
        );
    }

    #[test]
    fn ice_lite_controlling_does_not_nominate_on_a_binding_request() {
        // The controlled case above is the supported one. A *controlling*
        // ice-lite agent must not be promoted the same way: there,
        // `CheckState::Succeeded` feeds `evaluate_nomination`, so one
        // request — whose PRIORITY the sender chooses — would pick the
        // send address without any return-routability proof.
        let mut agent = new_test_agent();
        agent.set_ice_lite(true);
        agent.set_controlling(true);
        agent
            .add_local_candidate(Candidate::host(ipv4_1(), "udp").unwrap())
            .unwrap();

        let remote_creds = IceCreds::new();
        agent.set_remote_credentials(remote_creds.clone());

        let mut remote_candidate = Candidate::host(ipv4_3(), "udp").unwrap();
        remote_candidate.set_ufrag(&remote_creds.ufrag);
        let prio = remote_candidate.prio();
        agent.add_remote_candidate(remote_candidate);

        let serialized_req = make_serialized_binding_request(
            &agent.local_credentials,
            &remote_creds,
            false, // we are the controlling agent
            prio,
        );
        let binding_req = StunMessage::parse(&serialized_req).unwrap();

        assert!(agent.handle_packet(
            Instant::now(),
            StunPacket {
                message: binding_req,
                source: ipv4_3(),
                destination: ipv4_1(),
                proto: Protocol::Udp,
            },
        ));

        assert!(!agent.has_viable_remote_candidate(ipv4_3()));
        assert!(
            !agent
                .events
                .iter()
                .any(|e| matches!(e, IceAgentEvent::NominatedSend { .. })),
            "a single binding request must not choose the send address"
        );
    }

    #[test]
    fn controlled_sends_only_on_a_nominated_pair_whose_check_succeeded() {
        // RFC 8445 §7.3.1.5: the controlled agent acts on USE-CANDIDATE only
        // for a pair that is, or becomes, valid. Here the peer nominates the
        // highest-priority pair first, but our checks on it are never
        // answered (one-way connectivity). It then nominates a lower-priority
        // pair whose check succeeds. Media must go to the second pair.
        let mut agent = new_test_agent();
        agent.set_controlling(false);
        agent
            .add_local_candidate(Candidate::host(ipv4_1(), "udp").unwrap())
            .unwrap();

        let remote_creds = IceCreds::new();
        agent.set_remote_credentials(remote_creds.clone());

        let mut dead =
            Candidate::from_sdp_string("candidate:1 1 udp 2130706431 3.4.5.6 5000 typ host")
                .unwrap();
        dead.set_ufrag(&remote_creds.ufrag);
        let mut alive =
            Candidate::from_sdp_string("candidate:2 1 udp 1694498815 4.5.6.7 5000 typ host")
                .unwrap();
        alive.set_ufrag(&remote_creds.ufrag);
        let (dead_prio, alive_prio) = (dead.prio(), alive.prio());
        assert!(dead_prio > alive_prio);
        agent.add_remote_candidate(dead);
        agent.add_remote_candidate(alive);

        let nominate = |agent: &mut IceAgent, now: Instant, source: SocketAddr, prio: u32| {
            let username = format!("{}:{}", agent.local_credentials.ufrag, remote_creds.ufrag);
            let req = StunMessage::binding_request(&username, TransId::new(), true, 0, prio, true);
            let req = serialize_stun_msg(req, &agent.local_credentials.pass);
            assert!(agent.handle_packet(
                now,
                StunPacket {
                    message: StunMessage::parse(&req).unwrap(),
                    source,
                    destination: ipv4_1(),
                    proto: Protocol::Udp,
                },
            ));
        };

        // Answer our checks toward `alive` only; drop everything else.
        let answer_alive = |agent: &mut IceAgent, now: Instant| {
            let mut replies = vec![];
            while let Some(t) = agent.poll_transmit() {
                let msg = StunMessage::parse(&t.contents).unwrap();
                if t.destination == ipv4_4() && msg.is_binding_request() {
                    replies.push(make_authenticated_stun_reply(
                        msg.trans_id(),
                        ipv4_1(),
                        &remote_creds.pass,
                    ));
                }
            }
            for reply in replies {
                agent.handle_packet(
                    now,
                    StunPacket {
                        message: StunMessage::parse(&reply).unwrap(),
                        source: ipv4_4(),
                        destination: ipv4_1(),
                        proto: Protocol::Udp,
                    },
                );
            }
        };

        let nominated_sends = |agent: &mut IceAgent| {
            let mut v = vec![];
            while let Some(e) = agent.poll_event() {
                if let IceAgentEvent::NominatedSend { destination, .. } = e {
                    v.push(destination);
                }
            }
            v
        };

        let mut now = Instant::now();
        agent.handle_timeout(now);

        nominate(&mut agent, now, ipv4_3(), dead_prio);
        for _ in 0..20 {
            answer_alive(&mut agent, now);
            now += Duration::from_millis(50);
            agent.handle_timeout(now);
        }
        assert_eq!(
            nominated_sends(&mut agent),
            vec![],
            "a nominated pair whose check has not succeeded must not be used"
        );

        nominate(&mut agent, now, ipv4_4(), alive_prio);
        for _ in 0..20 {
            answer_alive(&mut agent, now);
            now += Duration::from_millis(50);
            agent.handle_timeout(now);
        }
        assert_eq!(nominated_sends(&mut agent), vec![ipv4_4()]);
    }

    fn make_serialized_binding_request(
        local_creds: &IceCreds,
        remote_creds: &IceCreds,
        controlling: bool,
        prio: u32,
    ) -> Vec<u8> {
        let username = format!("{}:{}", local_creds.ufrag, remote_creds.ufrag);
        let binding_req =
            StunMessage::binding_request(&username, TransId::new(), controlling, 0, prio, false);
        serialize_stun_msg(binding_req, &local_creds.pass)
    }

    fn make_authenticated_stun_reply(tx_id: TransId, addr: SocketAddr, password: &str) -> Vec<u8> {
        let reply = StunMessage::binding_reply(tx_id, addr);
        serialize_stun_msg(reply, password)
    }

    fn make_role_conflict_reply(tx_id: TransId, password: &str) -> Vec<u8> {
        let reply = StunMessage::binding_role_conflict_reply(tx_id);
        serialize_stun_msg(reply, password)
    }

    /// Serializing will calculate a message integrity for it. You can then re-parse to get a message
    /// that contains that correct integrity value.
    fn serialize_stun_msg(msg: StunMessage<'_>, password: &str) -> Vec<u8> {
        let mut buf = vec![0_u8; DATAGRAM_MTU_TARGET];

        let sha1_hmac =
            |key: &[u8], payloads: &[&[u8]]| DefaultSha1HmacProvider.sha1_hmac(key, payloads);
        let n = msg
            .to_bytes(Some(password.as_bytes()), &mut buf, sha1_hmac)
            .expect("IO error writing STUN message");
        buf.truncate(n);

        buf
    }

    #[test]
    fn set_mtu_updates_mtu() {
        let mut agent = IceAgent::new(IceCreds::new());
        assert_eq!(agent.mtu(), DATAGRAM_MTU_TARGET);
        assert_eq!(agent.mtu_warn(), DATAGRAM_MTU_WARN);

        agent.set_mtu(900..=1280);
        assert_eq!(agent.mtu(), 900);
        assert_eq!(agent.mtu_warn(), 1280);
    }
}
