# [S] A draining relay drains the cluster links it dialed

## Goal

A relay told to shut down sends GOAWAY on the cluster sessions it dialed, as it
does on those it accepted, and keeps them up until its drain deadline or until
the peer leaves. A neighbour then hears DRAIN and moves its routes before the
relay goes, instead of hearing only DEAD once the link drops, as after a crash.

## Plan

Found while checking route counters
([moq-dev/moq#9126](https://github.com/moq-dev/moq/pull/9126)) on a local
root/leaf0/leaf1 cluster over moq-lite-07: leaf1 dials both of its peers, and
on SIGINT its log reads "drain ended with every session gone" within
microseconds. Its neighbours count a DEAD and no DRAIN.
`rs/moq-relay/src/shutdown.rs` drains only accepted sessions; the dial loop in
`rs/moq-relay/src/cluster.rs` closes its sessions with the process.

Agent's recommendation: m3, not a blocker of the lite-07 cut. The mesh already
recovers from DEAD as it does from a crash; this only makes a planned restart
cheaper than an unplanned one.

- Hand the shutdown observer to the cluster dial loop, so it sends GOAWAY on
  its live session, stops redialing, and holds the drain open until the session
  ends or the shared deadline passes.
- Test it on real relays, in a triangle where the drained relay dials both
  others: each neighbour, hearing the GOAWAY itself, counts
  `routes.sent.drain` toward the other, which counts `routes.received.drain`,
  all before either counts a DEAD. No DRAIN comes from the draining relay
  itself, which sends GOAWAY: a pair of relays alone shows none.

Public API: none. Wire: none.

## Related

- [Real relays replay a mesh and its faults](/quest/m2/multicast/mesh-replay.md) - its roll drains each relay with GOAWAY
