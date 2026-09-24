# [M] Beacons and upstream tables

## Goal

Every cluster relay announces a beacon at `.internal/topology/<hop>`. Its
upstreams track publishes, per source relay, the relay's parent tie set and a
backup for each member. Forwarding is unchanged. The internal HTTP listener
shows the table and the sources whose backups are only link-protecting or
missing, so an operator can see weak spots in the mesh.

## Plan

- The beacon is a broadcast per relay, announced for the relay's life. It is
  always flooded, so every relay holds a best route and standby routes to
  every beacon.
- A link joins the upstream computation only after its session has been up
  continuously for a hold time, and leaves the moment it drops. A flapping
  peer never becomes anyone's parent. Drive the hold from tokio time so tests
  pause it.
- For each source `S` (the beacon's hop), derive:
  - the tie set `U_S`: neighbours whose beacon routes tie for best on
    every key of [rendezvous ranking](/quest/m1/announce-tree/route-order.md)
    before its hash keys;
  - for each member `u`, the backup `b_u`: the best standby whose chain avoids
    `u`, else the best on a session other than `u`'s, else none, with which
    kind it is. When `u` is `S` itself, only link protection is possible.
- Publish the table on the beacon's `upstreams` track whenever it changes.
  Entries name neighbours by hop id. Hop ids stay random per restart when
  unconfigured; a restarted relay is a new source.
- Keep it separate from the `.internal/origins` dial gossip
  (`rs/moq-relay/src/cluster.rs`). Static deployments such as moq.pro leave
  gossip off.

Test on an in-process four-relay ring with a chord:

- the tables match the hand-computed parents and backups;
- cutting a link updates them;
- a flapping session never enters them;
- a relay with random hop ids restarts cleanly.

## Required

- [Rendezvous ranking](/quest/m1/announce-tree/route-order.md) - the order
  the tie sets come from
