# [XS] Cross-relay bursts re-run

## Goal

Condition: the [#4349](https://github.com/moq-dev/moq/issues/4349) reporter
re-runs their A/B/C comparison (different nodes, one node, self-hosted)
against current cdn.moq.pro, and ideally shares their load harness, as asked
in the 2026-10-06 maintainer reply.

Check: a new comment on #4349 with the results, noting for each failed FETCH
whether it ended in an error or a timeout.

Once the condition clears, delete this quest. If the re-run still loses
groups or stalls cross-node, replace it with a concrete repro quest built on
their harness and move the `Closes` entry there; if it comes back clean,
close #4349 in the deletion PR.

## Plan

Established by a mock two-relay repro (one-frame groups in bursts of 14, a
subscription opened 5 s before the burst, fetch-on-gap with a 2 s deadline,
a flapping peer link), run against the 0.15.6 tree the reporter's clients
used and against `main`:

- **Fixed.** Burst-start misses (one more group lost per relay hop) and
  route-flap drops (about 70 of 560 groups) reproduce only on 0.15.6 and are
  gone on moq-net 0.3.8 and later, most likely by #4387.
- **Not reproduced.** Unanswered FETCHes and the 30 s `Stream(Old)` stalls
  showed up on no build, nor in the real-QUIC drill
  `rs/moq-relay/tests/drills.rs::bursts_cross_a_cluster` (bursts of 30
  across two clustered relays, every hop with 5% loss, delay, a 100 kbit/s
  bottleneck that each burst overflows, and 16-stream QUIC credit): every
  group arrives within about 3.5 s, and the occasional live `Old` reset is recovered by FETCH. Several
  30 s timers of that era have since been removed or shortened (#4606, #4741).
  That drill uses a lite peer link; over a `moq-transport-19` peer link, a
  related refusal does reproduce: FETCHes answered `old` for groups the origin
  still holds ([IETF peer FETCH refused old](/quest/m1/ietf-peer-fetch-old.md)).
- **Queueing.** FETCHes beyond a session's stream credit wait for it, one
  cross-relay round trip per slot, so with tight credit a gap's FETCH can
  outlast a 2 s deadline without being lost.
- **Expected.** Across relays a burst arrives newest-first, as the lite draft
  specifies; fetching on every gap at once multiplies FETCHes.

## Closes

- [#4349](https://github.com/moq-dev/moq/issues/4349) - close this issue when the quest finishes

## Related

- [Routes and announces](/quest/m1/cluster-routing/routes.md) - owns the stale and flapping announcements from the same report
