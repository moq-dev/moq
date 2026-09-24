# [M] Cluster simulator

## Goal

A deterministic, seeded simulator runs real `moq-net` origins, joined by
in-memory cluster sessions, through random meshes and event sequences. After
every step it checks the invariants tree forwarding must hold, and a failing
seed replays exactly. It runs in `just check` with a bounded seed count, and a
`just` recipe takes longer sweeps.

## Plan

Build it on real origins and sessions over in-process transports and paused
tokio time, not a model of them, so it tests the code that ships. Each seed
draws:

- a connected mesh of 4 to 40 relays, with prices including cost-0 siblings;
- publishers on one or more relays, some sharing a prefix (a warm carrier
  beside an origin);
- a sequence of events: link cuts and restores, relay kills and restarts,
  session flaps, drains, and relays set to flood-only or IETF to mimic a
  mixed-version rollout.

After each step settles, assert:

- **Completeness**: every relay that can reach a source of a prefix holds a
  route for it.
- **Bound**: a relay whose inbound peers all honor its table holds at most two
  copies of each route from a source. With flooding neighbours (lite-06,
  IETF, or flood-only), the bound is two plus one per flooding neighbour.
- **Failover**: a relay with a node-protecting backup keeps the route through
  its parent's failure, with in-place updates downstream and no end.
- **No loops**: no chain contains a hop twice.

Report per-event announce counts from the
[counters](/quest/m1/announce-tree/counters.md), with flooding as the
comparison. The simulator runs against today's flooding first: the invariants
hold there, except the bound. Tree forwarding then turns the bound on.

## Required

- [Announce counters](/quest/m1/announce-tree/counters.md) - the counts the
  simulator reports
