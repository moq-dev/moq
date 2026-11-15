# Route health

## Goal

A relay or endpoint that demands a track over one route watches a second
route to the same Instance with a metadata-only subscription, and moves the
track when the first stalls, without waiting on a QUIC keep-alive or idle
timeout. Today a route that dies without a close starves its readers until
the 10 s idle timeout fires, and a session that stays up while its payload
stalls is never recovered
(`rs/moq-net/tests/integration/route_health.rs`).

## Plan

Decided 2026-11-15 (maintainer, /quest-plan): ✅ planned again, in m3, as the
secondary route of [cache-aware multicast](/quest/m2/multicast/README.md)
(requirement 13: "don't rely on QUIC keep-alives for fast failovers"). The
first epic was deleted 2026-11-09 for want of a consumer
([moq-dev/moq#8423](https://github.com/moq-dev/moq/pull/8423)); read it with
`git show cd560bde5^:quest/m5/route-health/README.md`. Its decisions stand,
made by the maintainer on 2026-10-13 and 2026-11-02 from the stalls
[moq-dev/moq#7096](https://github.com/moq-dev/moq/pull/7096) measured:

- ✅ Evidence is comparison, never silence. Switch when the active route's
  delivered media time lags what the standby reports by more than the
  reader's max delay, or 2 s for a reader without one, while the reader
  demands. A quiet track is not a dead one, and `largest()` values are never
  compared.
- ✅ One standby per demanded track: the next-best route to the same
  Instance. Only payload demand creates a standby, so metadata observers
  never branch into a monitoring tree of their own.
- ✅ When neither route shows progress for that budget, the standby slot
  moves to the next candidate, doubling the wait per move, and nothing is
  failed.
- ✅ A demoted route never takes the track back while the working one
  delivers (2026-11-02, reversing 2026-10-13's return after recovery).
- ✅ Per track, not per session: one stalled track never fails a session.
- ✅ Only between claims of one Instance the publisher named. A minted
  Instance, as an older version or moq-transport gets, never resumes.

New on 2026-11-15, from the prior art this is closest to:

- The standby is a route whose path does not name this node, and one that
  does not cross the hop under suspicion ranks first. A fast-reroute
  alternate often shares the failed element (RFC 7431 section 7).
- A relay stops asking its standby when the standby is the only thing it
  serves that track to, so two relays never keep each other alive (the same
  RFC's rule for its secondary).
- The answer to a metadata-only subscription is also where a neighbour says
  whether it carries the Instance, if
  [warmth](/quest/m2/multicast/warmth.md) picks that.

This README's own work, once its children land: end-to-end validation across
a multi-hop relay network. Stalled video moves while healthy audio stays, a
stall with advancing control is caught, a recovered route does not take the
track back, two quiet routes stay unknown, a bottleneck shared by both routes
does not hide a working third, and relays deciding at once do not swap.
Report media overlap, control traffic, wakes, switches, and memory across
tracks, candidates, and readers. Revisit the relay's 10 s default QUIC idle
timeout with the result, and add a route-health section to the routing
concept page.
