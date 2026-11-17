# [L] What the route switch left

## Goal

The release cut publishes no route API that only tests call, the moq-cluster
draft and its moq-transport binding are gone, the route proof binds to the
router that ships, and no quest still describes the old route rule.

[moq-dev/moq#8987](https://github.com/moq-dev/moq/pull/8987) put every
lite-07 session on `route::relay::Router`, rewrote the draft, made `js/net`
an endpoint, and deleted the old rule with its route key. It left the items
below, each decided there or in the plan it came from.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit), and still
to do. The cut freezes the public API and the wire, so these land before it:

- ✅ The moq-cluster draft is retired with its moq-transport binding
  (rejected: reconciling the draft; an "out of date" note). If a third-party
  moq-transport relay turns out to negotiate the extension, rewrite the draft
  to what ships instead and say who. It is `drafts/draft-lcurley-moq-cluster.md`,
  `rs/moq-net/src/ietf/cluster.rs`, `js/net/src/ietf/cluster.ts`, its four
  code points (`rs/moq-net/src/ietf/mod.rs`), and its uses in `ietf/peer.rs`,
  `ietf/parameters.rs`, `ietf/publish_namespace.rs`,
  `ietf/subscribe_namespace.rs`, `ietf/session.rs`, `ietf/publisher.rs`, and
  `ietf/subscriber.rs`, about 300 sites in Rust and 240 in JS, most of them
  tests of hop paths between moq-transport peers. A moq-transport session
  then carries no hop path: it is an endpoint. Drop the extension from
  `doc/concept/standard.md`, `doc/bin/relay/cluster.md`, and the `max_hops`
  docs (`cluster::Config::max_hops`, `origin::Config::max_hops`,
  `doc/bin/relay/config.md`), which then bound lite-03 to lite-06 only. Run
  `just test interop --all`.
- ✅ The route items nothing outside moq-net calls become crate-private
  (rejected: `#[doc(hidden)]` until later; shipping them and breaking
  again). All in `rs/moq-net/src/model/origin.rs`: `Config::update_hold`,
  `Source`, `RouteInfo::source()`, and `Consumer::local()`. Kixel and moq.pro
  call none of them (checked 2026-11-17). Their callers are tests and benches
  outside the crate, which need another way in first:
  `rs/moq-net/tests/integration/mesh_withdraw.rs` and
  `benches/origin.rs` and `benches/auth.rs` set `update_hold`,
  `rs/moq-cli/src/play/media.rs` sleeps for it,
  `rs/moq-net/tests/integration/dial_split_horizon.rs`,
  `rs/moq-relay/tests/integration/auth_lifetime.rs`, and the tests of
  `rs/moq-relay/src/cluster.rs` read `source()` and `local()`. `Source::Peer(0)`
  means "named none", a special case inside a value, which AGENTS.md forbids
  in public API.

Left by the switch, for the implementer to decide and list in the PR:

- The announce-churn mitigations (`DEFAULT_UPDATE_HOLD` and
  `RETRACT_QUARANTINE` in `rs/moq-net/src/model/origin.rs`, and the same-clock
  dedup) sit in the announce cursor every version shares and carry no marker
  saying they are temporary. Gate what
  [acceptance](/quest/m2/cluster-routes/acceptance.md) does not retire on the
  lite-06 session path under one named marker.
- An adopted claim that crossed a marked link is withheld from a marked
  routing neighbour, but still floods to unmarked routing neighbours, who may
  forward it over their own marked links. Recommended: a second, marked
  identity and route for what a node adopts across a marked link, so the
  route rule's own mark carries it.
- The path-rule benches (drain, repath, follow) left `benches/routes.rs` with
  the old router and have no equivalent on the new one. Add them, swept over
  both axes.
- Costs the switch measured and left: re-offering a route sends its node's
  claims again; a non-routing peer is told one ANNOUNCE_UPDATE per broadcast
  when a route's cost changes; an edit swapping one prefix for another tells
  again the claims both cover; a relay whose only route closes ends its
  subscriptions while the claim stays shown.

Found 2026-11-16 by [the proof](https://github.com/moq-dev/moq/pull/8904)
against the prototype:

- Order equal routes by the neighbour's Node ID, or give the proof's binding
  the key the router orders them by: the proof orders them by the neighbour,
  the prototype by its simulation's node index, and `route::relay::Router` by
  its neighbour key.
- Record the chain as the claim instance's path: the announcing relay first,
  then each grant's exporter and adopter, so a node refuses a claim naming it
  in the role it would take. The prototype and the router record only the
  adopters, so they refuse fewer echoes than the proof assumes.
- Move the binding to the router: point `route/tests/algebra.rs` and
  `route/tests/sticky.rs` at the router's own functions for a node's own
  route, sending a route on, taking one in, ordering candidates, and listing
  them, regenerate `Vectors.agda`, and run `just test route-proof`. The claim
  instance first needs the router's precedence as its leading keys: a claim
  exactly at its path before a broader one shown there, then the newest
  epoch, decided over every claim of the Instance at that path whether or not
  a route reaches it, and only then the route joined with the claim's drain,
  priority, and cost. Neither the mechanized instance nor its Rust vectors
  cover precedence yet.

Quests written against the old wire, to retarget or delete:
[outbound federation](/quest/m3/cluster-routing/federation.md),
[the routing guide](/quest/m3/routing-guide.md),
[route wakes](/quest/m4/route-wakes.md),
[handoff routes](/quest/m4/relay-handoff-routes.md),
[regrant recheck](/quest/m4/regrant-route-recheck.md),
[announce interest memory](/quest/m4/announce-interest-memory.md),
[the mesh condition](/quest/m5/lite07-mesh.md),
[route memory](/quest/m9/perf/cluster-route-memory.md),
[route fan-out](/quest/m2/cluster-routes/route-fanout.md), and
[the Playwright bump](/quest/m1/playwright-pin.md), whose only reason was the
route key lane.

Public API: breaking. `Config::update_hold`, `Source`, `RouteInfo::source()`,
and `Consumer::local()` leave the public API. Wire: the moq-cluster extension
on moq-transport is no longer sent or read.

## Related

- [Cache-aware multicast](/quest/m2/multicast/README.md) - the epic
- [Acceptance](/quest/m2/cluster-routes/acceptance.md) - retires the update hold itself
- [The moq-net renames](/quest/m2/net-renames.md) - renames `Config::hop` and `with_preference`; whichever lands second merges
