# [M] Two-relay drill on impaired links

## Goal

A real-QUIC test runs a bursty small-group track (one frame per group, bursts
of tens of groups) from a publisher on one relay to a subscriber on a
clustered second relay, with the links impaired by loss, delay, and
flow-control pressure. It shows every group either arriving or failing by
name within a bounded time: no FETCH left unanswered, and no track stalled
by relay-hop `Old` resets. A failure it finds gets fixed at the cause, or a
quest of its own if large.

The mock two-relay harness behind
[#4349](https://github.com/moq-dev/moq/issues/4349) cannot produce those
resets, so the issue's unanswered FETCHes and 30 s `Stream(Old)` stalls are
still unexplained.

## Plan

`rs/moq-relay/tests/drills.rs` already runs single-relay drills over real
QUIC in a loopback and a `moq_shaper` impaired lane, and the cluster tests
(`cluster_idle.rs`, `goaway_cluster.rs`) start peered relays in-process.
Open for the planner:

- A new drill in that file's lanes or a separate two-relay test, and which
  hop or hops (client and peer links) get shaped.
- How to apply flow-control pressure: tight QUIC windows, a rate limit below
  the burst rate, or both.
- The subscriber's recovery model: fetch-on-gap with a deadline, as the
  reporter's app does, versus waiting and reordering locally.
- Whether a burst plus a flapping peer link joins as a regression for the
  0.15.6 losses fixed in moq-net 0.3.8.

## Related

- [Cross-relay bursts re-run](/quest/m1/cross-relay-bursts.md) - waits on the #4349 reporter's re-run against current cdn.moq.pro
- [Routes and announces](/quest/m1/cluster-routing/routes.md) - the cluster routing a flapping peer link exercises
