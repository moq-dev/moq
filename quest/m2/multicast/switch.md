# [XL] lite-07 sessions run the new route rule

## Goal

Every lite-07 session in Rust drives the
[new router](/quest/m2/multicast/router.md) on one announce stream per side,
the draft specifies it, and the old rule is gone: ROUTE_HOLD, the doubt,
DEAD's moved list, the Route Key Parameter with its proof and symbolic model,
qualified Node IDs, routes per prefix with their Route Suffix and Route
Depth, `path::Router`, and `RouteHandoff.tla`. The draft's routing text is
shorter than today's and the announce stream carries fewer message types.

`js/net` implements the endpoint half only: it picks among the routes it
receives and sends claims, and it holds no router and declares no Node ID.

The cut then publishes no route API or config that the old rule needed or
that nothing calls, and the moq-cluster draft, which specifies a routing
rule this one abandons, is retired with its moq-transport binding.

## Plan

Decided 2026-11-15 (maintainer, /quest-plan): lite-07 carries the new rule
and the cut waits for it ([epic](/quest/m2/multicast/README.md) item 2).
lite-07 is `-wip`, so relays pinned to `main` roll together, as each earlier
lite-07 wire change did.

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ The plan stands ([epic](/quest/m2/multicast/README.md) item 13;
  rejected: the route exchange as a separately versioned SETUP extension;
  the plan with a written fallback).
- ✅ `js/net` is an endpoint (rejected: porting the router to JS). The wire
  below already says an endpoint that never forwards declares no Node ID
  and sends claims, not routes; the Goal used to say every JS session
  "drives the new router". This is where JS's dedicated route code
  shrinks, about 2,270 lines by the audit's count
  (`js/net/src/lite/floors.ts`, `route_key.ts`, and the announce handling
  around them).
- ✅ The public route API and config the old rule needed are removed here,
  and the route items nothing outside moq-net calls become crate-private,
  listed under "Public surface" below (rejected: `#[doc(hidden)]` until
  later; shipping them and breaking again). The cut freezes whatever this
  PR leaves public.
- ✅ `moq_crypto::endpoint` goes with the Route Key, its only caller.
- ✅ The moq-cluster draft is retired with this PR (rejected: reconciling
  the draft; an "out of date" note). If a third-party moq-transport relay
  turns out to negotiate the extension, rewrite the draft to what ships
  instead and say who.

Recommended wire, for the implementer to confirm or improve; each departure
is a decision to list in the PR:

- A route is started, updated, and ended, and a closed session is named in
  one fixed-size message. Nothing answers a hold, and no message lists moved
  routes: the updates follow in order on the same stream.
- A claim carries its chain as a field of its own, replacing the temporary
  Lite-06 Chain and its Setup Parameter: the nodes that announced it again
  across a scoped grant or an older version.
- A request keeps its Instance and the origin node its first hop chose, and
  loses Route Depth. That ends the second subscription a move from a claim's
  route to its answer's opens on one link, which
  `route_failover::residual_two_depths_of_one_claim_subscribe_twice` pins
  ([moq-dev/moq#8881](https://github.com/moq-dev/moq/pull/8881)): flip it to
  one subscription.
- An endpoint that never forwards declares no Node ID. A peer holding a
  scoped publish grant sends claims and no routes, and the relay announces
  them under its own Node ID. Peers that trust each other with the whole
  tree exchange routes.
- The link mark stays as `upstream`. moq-net's per-link `proxy` switch, which
  the relay hard-wires on, goes if nothing else needs it; that is a public
  API change to report.
- Recommended by the audit, not decided: one identity per node. Once
  qualified Node IDs are gone, a Node ID and a Hop ID differ only when a
  Node ID rotates, which the draft requires when an origin's Revision would
  reach 2^64 - 1 (`drafts/draft-lcurley-moq-lite.md:518`, `:536`). No origin
  makes 2^64 changes. Drop the rotation rule and the Hop ID with it;
  ROUTE_START's Node ID is then the first hop of its path and can go too.
- Recommended, not decided: the Rendezvous Hash is normative FNV-1a with a
  private offset basis, `0x420C0DECB00B` (`draft:602`,
  `rs/moq-net/src/model/origin.rs:796`). Every node must rank alike, and an
  implementer who reaches for a stock FNV-1a will not. Use the standard
  basis, or add a sentence saying the basis is deliberate.
- Recommended, not decided: number the Announce Stream's message types
  densely when the set shrinks. `-wip` peers refuse each other by ALPN
  anyway, and the holes freeze otherwise.

Measured 2026-11-15 by the [bake-off](https://github.com/moq-dev/moq/pull/8880),
and decided 2026-11-16
([maintainer](https://github.com/moq-dev/moq/pull/8880#issuecomment-6911055514)),
for the wire:

- The closed-session message names the session and the node at its far end,
  and ends every route its sender offered through it, so no end follows per
  route.
- A draining node gets a fixed-size message of its own, sent once per
  neighbour: flagging each route cost 158,462 route messages for one GOAWAY on
  32 relays, against 1,454 with it and 116 today.
- A claim's end carries one bit, ended or lost, and a claim carries the lowest
  link priority to its publisher and a draining bit beside its cost and chain.
- A claims message with no change still carries its floor.
- A node offers a route to itself only once it announces a claim, so a relay
  that has never announced a claim and only forwards or only serves viewers
  has no route on the wire.
- A scoped neighbour is sent the node's closest claim per broadcast, and never
  one that came from that neighbour.
- Today's rule cost 300,100 route messages when a publisher left two meshes
  joined by two grants; the chain cost none. Keep that shape in the acceptance
  fixtures.

Delete with the old rule:

- `rs/moq-net/src/route/proof.rs`, `js/net/src/lite/route_key.ts`,
  `rs/moq-net/verify/route-key/`, the `verify-route-key` skill, and the
  interop route key lane (`test/interop/clients/js/route-key.ts`).
- The Route Key's public pieces, which the list above does not reach:
  `origin::RouteKey` (`rs/moq-net/src/route/mod.rs:35`),
  `origin::Config::route_key` (`rs/moq-net/src/model/origin.rs:112`), and
  the relay's `cluster.route_key` with `--cluster-route-key` and
  `MOQ_CLUSTER_ROUTE_KEY` (`rs/moq-relay/src/cluster.rs:741-748`, read at
  `:1471`). The key is new since release, so it needs no refusal.
- `moq_crypto::endpoint` (the persistent Ed25519 node key, its z-base-32
  ID, the `serde` feature, and `just rs crypto-key`): its one caller is the
  Route Key load at `rs/moq-relay/src/cluster.rs:1472`. A public key type
  for Kixel's trust layer is Kixel's to add.
- The moq-cluster extension: `drafts/draft-lcurley-moq-cluster.md` (361
  lines), `rs/moq-net/src/ietf/cluster.rs` (536), `js/net/src/ietf/cluster.ts`
  (263), its four code points (`rs/moq-net/src/ietf/mod.rs:99-114`), its
  uses in `ietf/peer.rs`, `ietf/parameters.rs`, and `publish_namespace`,
  and its mentions in `doc/concept/standard.md:53` and
  `doc/bin/relay/cluster.md`. The draft still tells a relay to advertise a
  lower cost for a namespace it already carries (line 52), the warm
  discount that formed pull cycles here, and lets a link cost 0 (line 88),
  which `route::Error::ZeroCost` refuses. The code implements neither, and
  item 10 of the epic makes a moq-transport session an endpoint, which has
  no path to carry. `cluster.max_hops` then bounds lite-03 to lite-06 only
  ([config names](/quest/m2/relay-config-names.md)).
- `rs/moq-net/verify/route/` and the `verify-route` skill, or rewrite the
  skill for the exhaustive search.
- [Firefox tabs prove their route key](/quest/m5/routes-browser-proof.md),
  and the release cut's reason to wait on
  [the Playwright bump](/quest/m1/playwright-pin.md).
- [Doubt holds](/quest/m2/cluster-routes/doubt-holds.md),
  [doubt and keep-alive](/quest/m2/cluster-routes/doubt-keepalive.md),
  [held after close](/quest/m2/cluster-routes/held-after-close.md), and
  whatever of [route fan-out](/quest/m2/cluster-routes/route-fanout.md) the
  new router answers, with the fixtures that pin them.

Public surface, to settle in this PR because the cut freezes it by
omission. All in `rs/moq-net/src/model/origin.rs`:

- `Config::update_hold` (`:155`): no reader or writer outside moq-net
  except one test sleep (`rs/moq-cli/src/play/media.rs:2478`). Crate-private
  now; [acceptance](/quest/m2/cluster-routes/acceptance.md) retires the
  hold itself.
- `Producer::with_proxy` (`:2404`): one caller,
  `rs/moq-relay/src/cluster.rs`, always `true`. Remove it or make it
  crate-private, as the wire bullet above says.
- `Source` (`:588`), `RouteInfo::source()` (`:704`), and
  `Consumer::local()` (`:7701`): called only under `#[cfg(test)]` in
  moq-relay. `Source::Peer(0)` means "named none", a special case inside a
  value, which AGENTS.md forbids in public API. Crate-private.
- Before hiding each, grep Kixel and moq.pro for a caller (the maintainer's
  condition for every unused-API removal) and keep what one uses.
- `Config::hop` is now `node` and `with_preference` is `with_priority`;
  this PR must not reintroduce the old names. The Route Offer's priority
  byte stays on the wire: the relay has no key to set it yet, and
  [outbound federation](/quest/m3/cluster-routing/federation.md) adds one.
- moq-ffi exposes only a route's `cost` and `available`
  (`rs/moq-ffi/src/origin.rs`), so no binding changes.

The announce-churn mitigations (`DEFAULT_UPDATE_HOLD` 300 ms at `:159`,
`RETRACT_QUARANTINE` 2 s at `:164`, and the same-clock dedup) sit in the
announce cursor every version shares and carry no marker saying they are
temporary. Whatever [acceptance](/quest/m2/cluster-routes/acceptance.md)
does not retire, this PR gates on the lite-06 session path under one named
marker, as the Lite-06 Chain is marked
(`rs/moq-net/src/lite/setup.rs:24-25`). The claim chain replaces the
Lite-06 Chain here, so update or delete that marker too: today it promises
a deletion at lite-08 that this plan makes permanent in another form.

The route plan says what an endpoint sends. Say in the draft what an
endpoint that never forwards must parse of routes, dead hops, and floors,
and what it may ignore: a third-party client implements that list.

Rewrite the assertions of the acceptance fixtures
(`tests/integration/route_acceptance.rs`, `route_failover.rs`,
`route_cluster.rs`) for the new rule, keeping their topologies.

Quests written against the old wire, to retarget or delete in this PR:
[outbound federation](/quest/m3/cluster-routing/federation.md),
[the routing guide](/quest/m3/routing-guide.md),
[route wakes](/quest/m4/route-wakes.md),
[handoff routes](/quest/m4/relay-handoff-routes.md),
[regrant recheck](/quest/m4/regrant-route-recheck.md),
[announce interest memory](/quest/m4/announce-interest-memory.md),
[the mesh condition](/quest/m5/lite07-mesh.md), and
[route memory](/quest/m9/perf/cluster-route-memory.md).

Update the draft's Announce, Floors, Routing, Dead Hops, and Resolution
sections, the message sections, the Setup Parameters, and the lite-07
changelog, and `doc/bin/relay/cluster.md`. Run `just check`,
`just test interop --all`, and `just drafts check`.

Public API: breaking. `origin::RouteKey`, `Config::route_key`, and
`moq_crypto::endpoint` are removed; `Config::update_hold`,
`Producer::with_proxy`, `Source`, `RouteInfo::source()`, and
`Consumer::local()` leave the public API. Config and CLI: `cluster.route_key`
is removed (never released). Wire: lite-07-wip only; the moq-cluster
extension on moq-transport is no longer sent or read.

## Required

- [The router](/quest/m2/multicast/router.md) - the rule these sessions drive
- [The route stream](/quest/m2/cluster-routes/route-stream.md) - one announce stream per side, which this builds on
