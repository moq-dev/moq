# Route health

## Goal

A relay or endpoint that opts in and demands a track over one route watches
a second route to the same Instance with a metadata-only subscription, and
moves the track when the first stalls, without waiting on a QUIC keep-alive
or idle timeout. Today a route that dies without a close starves its readers
until the 10 s idle timeout fires, and a session that stays up while its
payload stalls is never recovered
(`rs/moq-net/tests/integration/route_health.rs`).

## Plan

Decided 2026-11-15 (maintainer, /quest-plan): ✅ planned again, in m3, as the
secondary route of [cache-aware multicast](https://github.com/moq-dev/moq/blob/41cc107579/quest/m2/multicast/README.md)
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

Decided 2026-11-15 (maintainer, on
[moq-dev/moq#8873](https://github.com/moq-dev/moq/pull/8873)): ✅ opt in,
off by default: "We wouldn't do standbys inside our CDN". A standby about
doubles a track's subscription state and sends its headers down a second
path, which a mesh on links that stall wants and a CDN tier does not. The
node that would hold the standby turns it on, a relay in its configuration
and an endpoint on its client (rejected: a standby wherever a second route
exists). Turning it on for one broadcast, so an operator can offer it there,
is a later decision that [failover](/quest/m3/route-health/failover.md)
leaves room for. A node that has not opted in still finds a silent death by
the idle timeout alone and never recovers a stall on a live session.

Decided 2026-11-16 (maintainer, on
[moq-dev/moq#8882](https://github.com/moq-dev/moq/pull/8882)): ✅ the
metadata subscription and the payload subscription stay separate, and
nothing switches mode in place; the track moves as it moves between any two
routes. The epic is deferred behind the route layer: "we don't have an
immediate user".

Decided 2026-11-23 (maintainer, /quest-run): ✅ Kixel is the immediate user
the 2026-11-16 deferral lacked. Its robot failover
([kixel-ai/kixel.ai#242](https://github.com/kixel-ai/kixel.ai/pull/242)) fixes
a camera stuck on a silently dead LAN route through this epic's metadata
standby and [failover](/quest/m3/route-health/failover.md).

Decided 2026-11-25 (maintainer, /quest-run): ✅ "actually move
SUBSCRIBE_META entirely to m3 and out of lite-07". The metadata mode's wire
and serving are [wire.md](/quest/m3/route-health/wire.md), on lite-08-wip
after the lite-07 cut, and lite-07 freezes without the mode. So failover
waits on the lite-08 wire, and Kixel's failover with it (rejected, both
reversed: the wire in lite-07 before the cut so failover is not blocked on
lite-08, decided 2026-11-23; the serving side in m2 so every lite-07
publisher and relay serves the mode, decided earlier on 2026-11-25).
[meta.md](/quest/m3/route-health/meta.md) keeps the subscriber side, and
everything here stays in m3 (rejected: m2, m1, m0).

Decided 2026-11-25 (maintainer, /quest-run): ✅ nothing refuses the mode: a
relay whose upstream is lite-07 or earlier subscribes for payload and drops
it, and moq-transport gets [an extension](/quest/m3/route-health/ietf.md)
for the mode (rejected: a NOT_SUPPORTED refusal).

New on 2026-11-15, from the prior art this is closest to:

- The standby is a route whose path does not name this node, and one that
  does not cross the hop under suspicion ranks first. A fast-reroute
  alternate often shares the failed element (RFC 7431 section 7).
- A relay stops asking its standby when the standby is the only thing it
  serves that track to, so two relays never keep each other alive (the same
  RFC's rule for its secondary).
- The answer to a metadata-only subscription carries the neighbour's offer
  and data path for the Instance, as every subscribe answer does under
  [cache-aware routing](/quest/m3/cache-routing/select.md).

This README's own work, once its children land: end-to-end validation across
a multi-hop relay network. Stalled video moves while healthy audio stays, a
stall with advancing control is caught, a recovered route does not take the
track back, two quiet routes stay unknown, a bottleneck shared by both routes
does not hide a working third, and relays deciding at once do not swap.
Report media overlap, control traffic, wakes, switches, and memory across
tracks, candidates, and readers. Revisit the relay's 10 s default QUIC idle
timeout with the result, since it stays the only detector wherever the
standby is off, and add a route-health section to the routing concept page.
