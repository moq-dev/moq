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

Decided 2026-11-26 (maintainer, /quest-plan via kixel-ai/kixel.ai):

- ✅ Demote stale sessions automatically inside `moq-net` using the existing
  reversible `draining` rank, replacing `Grant::cost` and `Lease::update` live
  repricing (rejected: keeping `Grant::cost` in
  [moq-dev/moq#9696](https://github.com/moq-dev/moq/pull/9696); adding a local
  `Session::price()` handle plus a `moq-relay::Admission` session handle).
  `Offer.draining` (`DRAINING = 0x2`) already propagates across multi-hop
  `lite-07` meshes and is Agda-verified convergent; it recovers cleanly on
  `Live`, outranks all priorities and costs (including multi-hop and
  two-uplink paths) without a `STALE_COST` sentinel, and needs zero wire or
  `moq-auth` changes.
- ✅ On `lite-07` (`rs/moq-net/src/route/relay.rs`), track a per-link
  reversible `stale: bool` on `Peer` feeding `Offer.draining`
  (`peer.draining || peer.stale`) in `Here::arrive` and claim adoption
  (`Claim::adopted`, `restate`, and local ways), without sending
  `Msg::Drain { node }` (rejected: emitting `Msg::Drain`, which permanently
  poisons all paths through `node` for GOAWAY).
- ✅ On `lite-05/06` (`rs/moq-net/src/lite/subscriber.rs`) and `moq-transport`
  (`rs/moq-net/src/ietf/subscriber.rs`), live-update the existing `draining`
  flag on routes learned from the session on `Stale`/`Live` (reversible on
  `Live` while `going_away` is unset).
- ✅ Expose `pub fn draining(&self) -> bool` on `moq_net::origin::RouteInfo`
  and its `@moq/net` `RouteInfo` counterpart per Cross-Package Sync (rejected:
  keeping `RouteInfo.draining` crate-private).
- ✅ Expose `keep_alive` (`soft`, `hard`) config on `moq_tokio::quic::Config`
  (`[quic]`) and `moq-relay`'s `[limits.peer.quic]` (rejected: LAN-only
  config; forcing probing on all `[quic]` sessions including local clients;
  smuggling `keep_alive` into `moq-auth::Grant`).
- ✅ m1, since all changes are additive.

Guidance:

- `lite-07` (`rs/moq-net/src/route/relay.rs`): add a reversible `stale: bool`
  field beside `draining: bool` on `Peer`. Pass `peer.draining || peer.stale`
  into `Here::arrive`, and apply the same combined flag when adopting or
  restating claims given by that peer (`Claim::adopted`, `restate`, and local
  ways). Re-select affected origins and restate affected local claims when
  `stale` changes; keep `Msg::Drain { node }` exclusive to GOAWAY. A peer that
  also sent GOAWAY stays draining even if its liveness returns to `Live`.
- `lite-05/06` and `moq-transport`: drive `route.draining = going_away || stale`
  on liveness changes in `rs/moq-net/src/lite/subscriber.rs` and
  `rs/moq-net/src/ietf/subscriber.rs`, and mirror the reversible `draining`
  update in `@moq/net` (`js/net/src/lite/subscriber.ts`,
  `js/net/src/lite/routed.ts`, `js/net/src/ietf/subscriber.ts`).
- `RouteInfo::draining`: add `pub fn draining(&self) -> bool` on
  `moq_net::origin::RouteInfo` (`rs/moq-net/src/model/origin.rs`) and
  `readonly draining: boolean` on `RouteInfo` in `js/net/src/hop.ts` (included
  by `routeInfo` and no longer stripped in `js/net/src/origin.ts`). Deliver
  `draining` transitions to ordinary `origin.routes()` readers (updating
  `draining_is_invisible_to_readers`), while keeping wire forwarding unchanged
  for older protocols (`lite-05/06` and `moq-transport` still advertise real
  cost without a draining flag; only `lite-07` carries `DRAINING`).
- `moq-tokio` and `moq-relay` config: `moq_tokio::quic::Config`
  (`rs/moq-tokio/src/quic.rs`) already has `keep_alive: Option<cli::Duration>`
  for the single-duration NAT-binding ping interval (`--quic-keep-alive`).
  Keep the new `soft`/`hard` probing config additive so it coexists cleanly
  with `quic.keep_alive`, align its option shape with
  [Session keep-alive](/quest/m1/keep-alive.md), and mirror it on `moq-relay`'s
  `[limits.peer.quic]` (`rs/moq-relay/src/limits.rs` and `settings.rs`) so
  peer and dialed links can enable probing without touching local clients.
- Test multi-hop `lite-07` demotion, recovery to `Live`, and coexistence with
  GOAWAY; `lite-05/06` and `moq-transport` route re-ranking and recovery;
  `origin.routes()` observing `RouteInfo::draining()` transitions in Rust and
  JS; and `moq-relay` config merging and drill coverage for peer-link probing.

## Required

- [Session keep-alive with a stale signal](/quest/m1/keep-alive.md) - provides the per-session `keep_alive` (`soft`, `hard`) option and `Stale`/`Live` liveness watch

## Related

- [JS sessions take a link cost and priority](/quest/m1/js-session-cost.md) - static link pricing and priority
- [Cluster routing](/quest/m3/cluster-routing/README.md) - multi-hop `lite-07` route propagation and draining convergence
- [Route health](/quest/m3/route-health/README.md) - per-track failover complementary to session-wide stale demotion
