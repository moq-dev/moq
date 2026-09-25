# [M] Relay reachability and upstream tables

## Goal

Over the [cluster stream](/quest/m1/announce-tree/cluster-stream.md), every
relay learns its best route and a standby from each neighbour to every other
relay. From those it computes its upstream table and sends it to each
neighbour. Broadcast forwarding is unchanged. The internal HTTP listener shows
reachability, the table, and the sources whose backups are only
link-protecting or missing, so an operator can see weak spots in the mesh.

## Plan

- Each relay advertises itself at cost 0 with an empty chain. It forwards its
  best RELAY entry for each hop to every cluster peer except the one it came
  from, adding the link price and its own hop, as routes do today. Loop checks
  use the same chain rule. This is path-vector over `N` entries, flooded, and
  it changes only when relays or links do.
- Rank RELAY entries with the same comparator as
  [rendezvous ranking](/quest/m1/announce-tree/route-order.md), so a relay's
  parents match what route selection picks.
- A link counts only after its session has been up continuously for 10 s, the
  same threshold at which the cluster dial loop resets its backoff. It leaves
  the moment the session drops, so a flapping peer never becomes anyone's
  parent. Drive the hold from tokio time so tests pause it.
- For each source hop `S`, derive:
  - the tie set `U_S`: neighbours whose entries tie for best on every key
    before the hash keys;
  - for each member `u`, the backup `b_u`: the best standby whose chain
    avoids `u`, else the best on a session other than `u`'s, else none. When
    `u` is `S` itself, only link protection is possible.
- Send UPSTREAMS to each peer whenever the table changes. Hop ids stay random
  per restart when unconfigured; a restarted relay is a new source.

Test on an in-process four-relay ring with a chord:

- reachability and tables match the hand-computed parents and backups;
- cutting a link updates them;
- a flapping session never enters them;
- a relay restarting with a new random hop id converges cleanly.

## Required

- [Cluster stream](/quest/m1/announce-tree/cluster-stream.md) - the messages
  this sends
- [Rendezvous ranking](/quest/m1/announce-tree/route-order.md) - the order the
  tie sets come from
