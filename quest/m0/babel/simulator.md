# [M] Routing simulator

## Goal

A deterministic simulator runs cluster routing algorithms on the same
topologies and workloads, and reports announce cost and forwarding safety for
each. Its result decides whether the [Babel routing](/quest/m0/babel/README.md)
wire quests go ahead as written. No wire format or public API changes.

## Plan

Candidates:

- Today's path vector, as `rs/moq-net/src/model/origin.rs` and the lite
  publisher implement it, including hop exclusion and the "prefer shorter hop"
  tie-break.
- Babel as the line README specifies it: per-(source, prefix) seqno,
  feasibility with retention, the unreachable hold, ANNOUNCE_REFRESH, and
  adjacent-only exclusion.
- Topology split: relays share link costs, broadcasts announce only their
  origin relays, and a relay forwards an announcement to a peer only when it
  lies on that peer's shortest path to the origin.

The model is abstract (no QUIC, no codec), with a seeded scheduler that can
delay, reorder, and drop messages on a link. Each relay decides how it would
forward a subscribe or fetch at every step, using the same rules as serving:
specificity, anonymity, cost, adjacent exclusion, and per-request pool
selection. It flags any forwarding cycle and how long it lasts, and any
interval where a reachable broadcast has no route.

Workloads, swept over relay count and mesh degree:

- publish and unpublish, including a prefix covered by a broader one
- several publishers of one broadcast, and a warm relay re-originating it
- peer reconnect (full replay), link cost change, relay loss and restart
- the two counterexamples from #4213's review: a three-relay ring where a
  delayed same-seqno advertisement returns after withdrawal, and a relay
  falling back to a broader prefix that still routes through it

Report per algorithm and workload: announce messages and bytes by kind
(start, end, update), convergence time, loop and unavailability intervals,
and retained state per relay. Write the findings into the line README's Gate
and adjust the wire quests to match.

It lives in an unpublished workspace crate (`publish = false`). The workloads
run as tests in nightly CI; the sweep is a benchmark. The scenarios later
become regression tests against the real implementation.

## Related

- [Announce counters](/quest/m0/announce-counters.md) - live numbers to check the simulated baseline against
