use std::collections::VecDeque;
use std::fmt;
use std::time::{Duration, Instant};

use crate::Candidate;
use crate::CandidateKind;
use crate::stun::{DEFAULT_MAX_RETRANSMITS, StunTiming, TransId};
use str0m_proto::Id;
use str0m_proto::Pii;

// When running ice-lite we need a cutoff when we consider the remote definitely gone.
const RECENT_BINDING_REQUEST: Duration = Duration::from_secs(15);

/// A pair of candidates, local and remote, in the ice agent.
pub struct CandidatePair {
    id: PairId,

    /// Index into the local_candidates list in IceAgent.
    local_idx: usize,
    local_kind: CandidateKind,

    /// Index into the remote_candidates list in IceAgent.
    remote_idx: usize,
    remote_kind: CandidateKind,

    /// Index into local_candidates for the last successful
    /// response. This forms the "valid pair" logic in the spec.
    valid_idx: Option<usize>,

    /// Calculated prio given the candidates.
    prio: u64,

    /// Current state of this pair. Start in Waiting (there is
    /// no frozen state since there is only one data stream).
    state: CheckState,

    /// Record of the latest STUN messages we've tried using this pair.
    ///
    /// This list will usually not grow beyond [`DEFAULT_MAX_RETRANSMITS`] * 2
    /// unless the user configures a very large retransmission counter.
    binding_attempts: VecDeque<BindingAttempt>,

    /// The next time we are to do a binding attempt, cached, since we
    /// potentially recalculate this many times per second otherwise.
    cached_next_attempt_time: Option<Instant>,

    /// Number of remote binding requests we seen for this pair.
    remote_binding_requests: u64,

    /// Last remote binding request.
    remote_binding_request_time: Option<Instant>,

    /// State of nomination for this candidate pair.
    nomination_state: NominationState,

    /// Total number of STUN binding responses received on this pair,
    /// across the whole pair lifetime (not bounded by `binding_attempts`).
    responses_received: u64,

    /// Sum of all RTTs measured from successful STUN binding transactions
    /// on this pair, across the whole pair lifetime.
    total_round_trip_time: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CheckState {
    /// A check has not been sent for this pair.
    #[default]
    Waiting,

    /// A check has been sent for this pair, but the
    /// transaction is in progress.
    InProgress,

    /// A check has been sent for this pair, and it produced a
    /// successful result.
    Succeeded,
    //
    // Hey martin, this looks fishy you might say. Why is there no
    // failed state? The reason is that agent removes the candidatepair
    // straight away if it is deemed failed.
    //
    // /// A check has been sent for this pair, and it failed (a
    // /// response to the check was never received, or a failure response
    // /// was received).
    // Failed,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum NominationState {
    /// This pair has not been nominated.
    #[default]
    None,
    /// The pair has been nominated. The binding request for the
    /// nomination has not been sent.
    Nominated,
    /// The binding request is sent.
    Attempt,
    /// Successful nomination. Received a reply for the transaction id.
    Success,
}

pub struct BindingAttempt {
    /// The transaction id used in the STUN binding request.
    ///
    /// This is how we recognize the binding response.
    trans_id: TransId,

    /// The time we sent the binding request.
    request_sent: Instant,

    /// The time we got a binding response, if ever.
    respone_recv: Option<Instant>,

    /// Whether the binding attempt is nominated.
    nominated: bool,

    /// The agent's `controlling` value at request time.
    ///
    /// Used to decide whether a 487 (Role Conflict) response is
    /// stale: if the agent has already swapped role since the
    /// request was sent, the 487 must be ignored to avoid flapping
    /// when in-flight requests with the old role get rejected.
    controlling: bool,
}

impl BindingAttempt {
    pub fn controlling(&self) -> bool {
        self.controlling
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PairId([u8; 20]);

impl Default for PairId {
    fn default() -> Self {
        PairId(Id::random().into_array())
    }
}

impl CandidatePair {
    pub fn new(
        local_idx: usize,
        local_kind: CandidateKind,
        remote_idx: usize,
        remote_kind: CandidateKind,
        prio: u64,
    ) -> Self {
        CandidatePair {
            local_idx,
            local_kind,
            remote_idx,
            remote_kind,
            prio,
            binding_attempts: VecDeque::with_capacity(DEFAULT_MAX_RETRANSMITS * 2),
            id: Default::default(),
            valid_idx: Default::default(),
            state: Default::default(),
            cached_next_attempt_time: Default::default(),
            remote_binding_requests: Default::default(),
            remote_binding_request_time: Default::default(),
            nomination_state: Default::default(),
            responses_received: 0,
            total_round_trip_time: Duration::ZERO,
        }
    }

    pub fn id(&self) -> PairId {
        self.id
    }

    pub fn calculate_prio(controlling: bool, remote_prio: u32, local_prio: u32) -> u64 {
        // The ICE agent computes a priority for each candidate pair.  Let G be
        // the priority for the candidate provided by the controlling agent.
        // Let D be the priority for the candidate provided by the controlled
        // agent.  The priority for a pair is computed as follows:
        //
        //    pair priority = 2^32*MIN(G,D) + 2*MAX(G,D) + (G>D?1:0)

        let (g, d) = if controlling {
            (local_prio, remote_prio)
        } else {
            (remote_prio, local_prio)
        };

        2_u64.pow(32) * g.min(d) as u64 + 2 * g.max(d) as u64 + if g > d { 1 } else { 0 }
    }

    pub fn local_idx(&self) -> usize {
        self.local_idx
    }

    pub fn remote_idx(&self) -> usize {
        self.remote_idx
    }

    pub fn local_candidate<'a>(&self, cs: &'a [Candidate]) -> &'a Candidate {
        &cs[self.local_idx]
    }

    pub fn remote_candidate<'a>(&self, cs: &'a [Candidate]) -> &'a Candidate {
        &cs[self.remote_idx]
    }

    pub fn prio(&self) -> u64 {
        self.prio
    }

    pub fn set_prio(&mut self, prio: u64) {
        self.prio = prio;
    }

    pub fn state(&self) -> CheckState {
        self.state
    }

    pub fn increase_remote_binding_requests(&mut self, now: Instant) {
        self.remote_binding_requests += 1;
        self.remote_binding_request_time = Some(now);
        trace!("Recorded binding request: {:?}", self);
    }

    pub fn remote_binding_request_time(&self) -> Option<Instant> {
        self.remote_binding_request_time
    }

    pub fn is_ice_lite_alive(&self, now: Instant) -> bool {
        let Some(t) = self.remote_binding_request_time else {
            return true; // No timestamp yet = hasn't had a chance to fail
        };
        now - t < RECENT_BINDING_REQUEST
    }

    pub fn is_nominated(&self) -> bool {
        !matches!(self.nomination_state, NominationState::None)
    }

    pub fn nominate(&mut self, force_success: bool) {
        assert!(self.nomination_state == NominationState::None);
        if force_success {
            self.nomination_state = NominationState::Success;
            debug!("Force success nominated pair {:?}", Pii(&self));
        } else {
            self.nomination_state = NominationState::Nominated;
            debug!("Nominated pair: {:?}", Pii(&self));
        }
    }

    pub fn copy_nominated_and_success_state(&mut self, other: &CandidatePair) {
        match other.nomination_state {
            NominationState::Nominated | NominationState::Success => {
                self.nomination_state = other.nomination_state;
            }
            // None is the default, no need to copy
            NominationState::None => {}
            // The old pair had a nominated binding request in flight. We can't copy
            // Attempt directly because the new pair doesn't have the corresponding
            // transaction ID in its binding_attempts. Reset to Nominated so the next
            // binding cycle issues a fresh attempt that can promote back to Success.
            NominationState::Attempt => {
                self.nomination_state = NominationState::Nominated;
            }
        }

        // The replacement has the same local base and remote address, so a check
        // that succeeded on the old pair validated this path too. An in-progress
        // check is not copied, for the same reason as Attempt above.
        if other.state == CheckState::Succeeded {
            self.state = CheckState::Succeeded;
        }
    }

    /// Records a new binding request attempt.
    ///
    /// Returns the transaction id to use in the STUN message.
    pub fn new_attempt(
        &mut self,
        now: Instant,
        timing_config: &StunTiming,
        controlling: bool,
    ) -> TransId {
        // calculate a new time
        self.cached_next_attempt_time = None;

        if matches!(self.nomination_state, NominationState::Nominated) {
            debug!("Nominated attempt STUN binding: {:?}", Pii(&self));
            self.nomination_state = NominationState::Attempt;
        }

        let attempt = BindingAttempt {
            trans_id: TransId::new(),
            request_sent: now,
            respone_recv: None,
            nominated: self.is_nominated(),
            controlling,
        };

        self.binding_attempts.push_back(attempt);

        // Never keep more than the maximum allowed retransmits.
        while self.binding_attempts.len() > timing_config.max_retransmits() {
            self.binding_attempts.pop_front();
        }

        if self.state == CheckState::Waiting {
            trace!(
                "Check state: {:?} -> {:?}",
                self.state,
                CheckState::InProgress
            );
            self.state = CheckState::InProgress;
        }

        trace!("Sending binding request: {:?}", self);

        let last = self.binding_attempts.back().unwrap();

        last.trans_id
    }

    /// Returns the binding attempt for the given STUN transaction id, if
    /// this pair sent it.
    pub fn binding_attempt(&self, trans_id: TransId) -> Option<&BindingAttempt> {
        self.binding_attempts
            .iter()
            .find(|b| b.trans_id == trans_id)
    }

    /// Marks a binding request attempt as having a successful response.
    ///
    /// ### Panics
    ///
    /// Panics if the trans_id doesn't belong to this pair.
    pub fn record_binding_response(&mut self, now: Instant, trans_id: TransId, valid_idx: usize) {
        self.cached_next_attempt_time = None;

        self.valid_idx = Some(valid_idx);

        let attempt = self
            .binding_attempts
            .iter_mut()
            .find(|b| b.trans_id == trans_id)
            .expect("Binding request attempt");

        attempt.respone_recv = Some(now);

        self.responses_received += 1;
        self.total_round_trip_time += now - attempt.request_sent;

        if attempt.nominated && self.nomination_state == NominationState::Attempt {
            self.nomination_state = NominationState::Success;
            debug!("Nomination success: {:?}", Pii(&self));
        }

        if self.state == CheckState::InProgress {
            trace!(
                "Check state: {:?} -> {:?}",
                self.state,
                CheckState::Succeeded
            );
            self.state = CheckState::Succeeded;
        }

        trace!("Recorded binding response: {:?}", self);
    }

    /// Marks the pair as validated by an incoming binding request we answered.
    ///
    /// This is for ice-lite only. A lite agent sends no checks of its own, so
    /// [`CandidatePair::record_binding_response`] never runs for it and the
    /// pair would stay `Waiting` for its entire life.
    ///
    /// This is weaker than [`CandidatePair::record_binding_response`] and
    /// says so: a binding *response* proves return routability, while an
    /// answered request proves only that the sender knows our password. An
    /// ice-lite agent cannot obtain the former by construction, so the
    /// alternative is not a stronger check but a permanently unusable
    /// state. The caller restricts this to controlled agents, where
    /// nomination — which is what governs egress — already accepts a
    /// forged request without return-routability proof.
    pub fn record_remote_binding_request_success(&mut self) {
        if self.state != CheckState::Succeeded {
            trace!(
                "Check state: {:?} -> {:?}",
                self.state,
                CheckState::Succeeded
            );
            self.state = CheckState::Succeeded;
        }
    }

    /// The time of the last binding request attempt.
    ///
    /// `None` means there has been no attempts.
    fn last_attempt_time(&self) -> Option<Instant> {
        self.binding_attempts.back().map(|b| b.request_sent)
    }

    /// The round-trip time of the most recent successful STUN binding
    /// transaction recorded for this pair.
    ///
    /// `None` if no binding response has been received yet.
    pub fn last_successful_rtt(&self) -> Option<Duration> {
        self.binding_attempts
            .iter()
            .rev()
            .find_map(|b| b.respone_recv.map(|r| r - b.request_sent))
    }

    /// Total number of STUN binding responses received on this pair over
    /// the pair's lifetime.
    pub fn responses_received(&self) -> u64 {
        self.responses_received
    }

    /// Sum of all RTTs measured from successful STUN binding transactions
    /// on this pair over the pair's lifetime.
    pub fn total_round_trip_time(&self) -> Duration {
        self.total_round_trip_time
    }

    /// From the back of binding_attempts, go through all unanswered and find
    /// the time when we started having an outage.
    ///
    /// The returned value is the number of unanswered attempts and the oldest time.
    fn unanswered(&self) -> Option<(usize, Instant)> {
        self.binding_attempts
            .iter()
            .rev()
            .take_while(|b| b.respone_recv.is_none())
            .enumerate()
            .last()
            .map(|(idx, b)| (idx + 1, b.request_sent))
    }

    /// When we should do the next retry.
    pub fn next_binding_attempt(&mut self, now: Instant, timing_config: &StunTiming) -> Instant {
        if let Some(cached) = self.cached_next_attempt_time {
            return cached;
        }

        let next = if matches!(self.nomination_state, NominationState::Nominated) {
            // Cheating a bit to make the nomination "skip the queue".
            // Must handle underflow gracefully, machine may be running for < 60s.
            now.checked_sub(Duration::from_secs(60)).unwrap_or(now)
        } else if let Some(last) = self.last_attempt_time() {
            // When we have unanswered for longer than STUN_MAX_RTO_MILLIS / 2, start
            // checking more often.
            let unanswered_count = self
                .unanswered()
                .filter(|(_, since)| now - *since > timing_config.max_rto() / 2)
                .map(|(count, _)| count);

            let send_count = unanswered_count.unwrap_or(self.binding_attempts.len());

            last + timing_config.stun_resend_delay(send_count)
        } else {
            // No previous attempt, do next retry straight away.
            now
        };

        // At least do a check at this time.
        let min = now + timing_config.max_rto();

        let at_least = next.min(min);

        // keep this cached since the calculation can happen very often.
        self.cached_next_attempt_time = Some(at_least);

        at_least
    }

    /// Tells if this candidate pair is still possible to use for connectivity.
    ///
    /// Returns `false` if the candidate has failed.
    pub fn is_still_possible(&self, now: Instant, timing_config: &StunTiming) -> bool {
        // If we've recently received a binding request from the remote peer, the pair
        // is still viable even if our own binding requests aren't getting responses.
        // This handles the case where the remote peer is still sending us requests
        // (indicating connectivity) but may not be responding to our requests due to
        // asymmetric behavior in some browser versions (e.g., Chrome v144+).
        if let Some(t) = self.remote_binding_request_time {
            if now - t < RECENT_BINDING_REQUEST {
                return true;
            }
        }

        let attempts = self.binding_attempts.len();
        let unanswered = self.unanswered().map(|b| b.0).unwrap_or(0);

        if attempts < timing_config.max_retransmits()
            || unanswered < timing_config.max_retransmits()
        {
            true
        } else {
            // check to see if we are still waiting for the last attempt
            // this unwrap is fine because unanswered count > 0
            let last = self.last_attempt_time().unwrap();
            let cutoff = last + timing_config.stun_last_resend_delay();
            now < cutoff
        }
    }

    pub(crate) fn copy_remote_binding_requests(&mut self, other: &CandidatePair) {
        self.remote_binding_requests = other.remote_binding_requests;
        self.remote_binding_request_time = other.remote_binding_request_time;

        // ICE-lite pairs are validated by incoming requests. Preserve that
        // validation when a higher-priority candidate replaces the pair.
        if other.state == CheckState::Succeeded {
            self.state = CheckState::Succeeded;
        }
    }

    pub(crate) fn update_kinds(
        &mut self,
        local_candidates: &[Candidate],
        remote_candidates: &[Candidate],
    ) {
        self.local_kind = local_candidates[self.local_idx].kind();
        self.remote_kind = remote_candidates[self.remote_idx].kind();
    }

    pub(crate) fn reset_cached_next_attempt_time(&mut self) {
        self.cached_next_attempt_time = None;
    }

    #[cfg(test)]
    pub fn remote_binding_requests(&self) -> (u64, Option<Instant>) {
        (
            self.remote_binding_requests,
            self.remote_binding_request_time,
        )
    }
}

impl PartialEq for CandidatePair {
    fn eq(&self, other: &Self) -> bool {
        self.local_idx == other.local_idx
            && self.remote_idx == other.remote_idx
            && self.prio == other.prio
    }
}

impl Eq for CandidatePair {}

impl PartialOrd for CandidatePair {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(Self::cmp(self, other))
    }
}

impl Ord for CandidatePair {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // reverse since we want highest prio first.
        self.prio.cmp(&other.prio).reverse()
    }
}

impl fmt::Debug for BindingAttempt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BindingAttempt")
            .field("request_sent", &self.request_sent)
            .field("respone_recv", &self.respone_recv)
            .finish()
    }
}

impl fmt::Debug for CandidatePair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "CandidatePair(\
                {}-{} ({}-{}) prio={} state={:?} attempts={} unanswered={} remote={} nom={:?}\
            )",
            self.local_idx,
            self.remote_idx,
            self.local_kind,
            self.remote_kind,
            self.prio,
            self.state,
            self.binding_attempts.len(),
            self.unanswered().map(|b| b.0).unwrap_or(0),
            self.remote_binding_requests,
            self.nomination_state
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ice_timeout_after_established() {
        let stun_timing = StunTiming::default();

        let mut pair = CandidatePair::new(0, CandidateKind::Host, 0, CandidateKind::Host, 0);

        let mut now = Instant::now();

        for _ in 0..20 {
            let id = pair.new_attempt(now, &stun_timing, false);

            now += Duration::from_millis(500);

            pair.record_binding_response(now, id, 0);
        }

        let offline_at = now;

        while pair.is_still_possible(now, &stun_timing) {
            pair.new_attempt(now, &stun_timing, false);
            now = pair.next_binding_attempt(now, &stun_timing);
        }

        let duration = now.duration_since(offline_at);

        assert_eq!(duration, stun_timing.timeout())
    }

    #[test]
    fn last_successful_rtt_returns_most_recent() {
        let stun_timing = StunTiming::default();
        let mut pair = CandidatePair::new(0, CandidateKind::Host, 0, CandidateKind::Host, 0);
        let now = Instant::now();

        assert_eq!(pair.last_successful_rtt(), None);
        assert_eq!(pair.responses_received(), 0);
        assert_eq!(pair.total_round_trip_time(), Duration::ZERO);

        let id1 = pair.new_attempt(now, &stun_timing, false);
        pair.record_binding_response(now + Duration::from_millis(50), id1, 0);
        assert_eq!(pair.last_successful_rtt(), Some(Duration::from_millis(50)));
        assert_eq!(pair.responses_received(), 1);
        assert_eq!(pair.total_round_trip_time(), Duration::from_millis(50));

        // Newer in-flight attempt without response shouldn't shadow the
        // older successful one or change cumulative counters.
        let _id2 = pair.new_attempt(now + Duration::from_millis(100), &stun_timing, false);
        assert_eq!(pair.last_successful_rtt(), Some(Duration::from_millis(50)));
        assert_eq!(pair.responses_received(), 1);

        // A newer successful attempt replaces current rtt and accumulates totals.
        let id3 = pair.new_attempt(now + Duration::from_millis(200), &stun_timing, false);
        pair.record_binding_response(now + Duration::from_millis(220), id3, 0);
        assert_eq!(pair.last_successful_rtt(), Some(Duration::from_millis(20)));
        assert_eq!(pair.responses_received(), 2);
        assert_eq!(pair.total_round_trip_time(), Duration::from_millis(70));
    }
}
