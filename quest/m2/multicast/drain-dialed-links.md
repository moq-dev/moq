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

Decided 2026-11-19 (coordinator, under the maintainer's pick that day to
merge on the coordinator's recommendation): ✅ kept, and under this epic, so
the lite-07 cut waits on it (rejected: m3, as the implementer recommended;
dropping it). A deployment that rolls one relay at a time restarts every
relay on purpose, and until this lands each of those restarts is a crash to
the neighbours the relay dialed.
The mesh replay (`just test mesh-replay`) measures a roll, and without this
it measures crashes: its roll table reads DEAD on every link the drained
relay dialed (starred), and this fix moves each to DRAIN.

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

- `test/mesh-replay/` - its roll drains each relay with GOAWAY and reports DRAIN or DEAD per link
