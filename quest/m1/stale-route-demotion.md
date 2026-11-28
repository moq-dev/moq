# [M] Demote routes on stale sessions and expose draining

## Goal

A session whose keep-alive liveness turns `Stale` automatically demotes its
routes using the existing reversible `draining` rank, and restores them when
the session returns to `Live`, without closing the session or changing the
wire protocol or `moq-auth`.

- On `lite-07`, a stale peer link marks routes learned through that link
  draining across the mesh and clears the mark when it recovers, without
  emitting `Msg::Drain`.
- On `lite-05/06` and `moq-transport`, routes learned from a stale session
  flip `draining` on and off live, following the same path as `going_away`.
- `moq_net::origin::RouteInfo` exposes `pub fn draining(&self) -> bool`
  (mirrored on `@moq/net`'s `RouteInfo`), so downstream callers can ignore a
  stalled-only peer or surface draining routes in status without per-session
  liveness plumbing.
- `moq_tokio::quic::Config` (`[quic]`) and `moq-relay`'s `[limits.peer.quic]`
  expose `keep_alive` (`soft`, `hard`) configuration so relays and embedders
  can enable probing on peer and dialed links without forcing it onto local
  client sessions.

## Plan

Implemented in [moq-dev/moq#9838](https://github.com/moq-dev/moq/pull/9838) (draft):
reversible lite-07 `Peer.stale`, reversible lite-05/06 and moq-transport route
draining driven by `Session` liveness, `RouteInfo::draining()` in Rust and
`RouteInfo.draining` in `@moq/net`, `[quic] keep_alive_soft`/`keep_alive_hard`,
and `[limits.peer.quic]` mirrors applied to accepted peers and dialed links.

Remaining before it lands:

- A `moq-relay` drill (`rs/moq-relay/tests/drills.rs`) that collapses a peer link
  into a full queue and watches its routes drain and recover. End-to-end coverage
  today is `moq-tokio`'s simulator test `sim::stale_routes_drain`.
- A full `just check` on the final head, and `/quest-review`.

## Waiting

- Maintainer: should `@moq/net` also flip routes' `draining` on stale sessions, as the plan asked, when JS has no keep-alive or liveness signal to drive it?
  - Skip until JS gains a keep-alive (Recommended) - nothing in JS can report staleness, so the plumbing would be dead code; `RouteInfo.draining` is mirrored already
  - Add the reversible plumbing now - ready for a future JS keep-alive, at the cost of untested code
- Maintainer: should the bindings (`moq-ffi` `MoqRouteInfo`, libmoq) expose `draining` too?
  - Not until a binding user asks (Recommended) - it touches every SDK wrapper and libmoq's C ABI, and binding listings report only present/absent
  - Add it in this PR - keeps every language in step with Rust and JS

## Related

- [Cluster routing](/quest/m3/cluster-routing/README.md) - multi-hop `lite-07` route propagation and draining convergence
- [Route health](/quest/m3/route-health/README.md) - per-track failover complementary to session-wide stale demotion
