# [L] Cluster topology

## Goal

Relays learn the relay graph once, from a cluster message kept apart from
routes, instead of every route repeating how to reach its origin. Each relay
knows every configured link, whether it is up, and its cost, and computes its
shortest path to every other relay. No announcement is needed to learn that
relay K exists or how to reach it.

## Plan

Decided: the topology is configured. `--cluster-connect` or the connect API
gives the relay graph and link costs, and LAN mDNS dials peers that then count
as configured links. Gossip discovery is gone first, by
[Remove gossip](/quest/m0/remove-gossip.md).

Candidate mechanics, from the simulator (see the questline README's
findings):

- Relays flood per-link liveness among themselves with a per-link seqno. The
  seqno is scoped to the relay's incarnation, so a restarted relay's links
  supersede its stale ones instead of looking older.
- A relay batches the liveness reports it sends, its own and those it
  forwards, for a short hold-down (50 ms in the simulator), and recomputes its
  shortest paths after a matching delay, as OSPF's SPF delay does. Unbatched,
  one relay restart at 340 relays sent half a million messages; batched, 27k.
- On session up, relays exchange a digest (each reporter's incarnation and
  its seqno per link) and send only what the other lacks. A reporter's
  reports flood separately, so its newest seqno alone would hide a missing
  older one. The digest already counts the fresh report of the link that just
  came up; sending the database first replays the relay's own report from
  when the link went down, and the peer drops the link it is using.
- Distance compares cost, then hop count, so every hop strictly shortens it
  even across `?cost=0` links.

Reduced flooding ([RFC 9667](https://www.rfc-editor.org/rfc/rfc9667)) is
out of scope; [Propagation](/quest/m1/cluster-routing/propagation.md) writes
it as an m2 quest if it stays deferred.

Wire: new cluster-session messages in the current wip lite version, with the
draft updated in the same PR. Tests drive topologies in process with mocked
time, including restart, a link flapping, and a digest racing a link that
just came up. Expose the computed graph where operators already look
(`/nodes` in `rs/moq-relay/src/internal.rs`).

## Required

- [Remove gossip](/quest/m0/remove-gossip.md) - configured links are the only topology source

## Related

- [Cluster idle timeout](/quest/m1/cluster-idle-timeout.md) - liveness only reports what the session layer detects
- [Drain](/quest/m1/drain/README.md) - a second relay per PoP joins this topology
