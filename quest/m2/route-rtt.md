# [S] Decide whether ROUTE keeps its RTT

## Goal

Before lite-07 freezes, the maintainer settles whether ROUTE carries an
informational RTT now that routes name their path, and the draft, Rust, JS,
and the bindings match.

## Plan

Found 2026-11-14 by the [feature review](/quest/m3/release/feature-review.md).
[moq-dev/moq#8458](https://github.com/moq-dev/moq/pull/8458) and
[moq-dev/moq#8553](https://github.com/moq-dev/moq/pull/8553) carry a summed
RTT because, per the deleted route-latency quest, a lite-07 route "cannot name
its path, so a sum is all the wire can carry". Since
[moq-dev/moq#8487](https://github.com/moq-dev/moq/pull/8487) every route
carries its hops. In `path::Router::rtt` (`rs/moq-net/src/route/path.rs`), one
link's RTT change past max(5 ms, 25%) recomputes every identity heard through
that neighbour and sends one ROUTE_UPDATE per publisher on every downstream
stream, repeating at each relay down; nothing ranks or selects by it, the
sizing covered chain length for one origin only (AGENTS.md asks for a sweep
over both axes), the "a node may withhold it" requirement never shipped, and
its in-repo consumer, link quality, moved downstream
([moq-dev/moq#8756](https://github.com/moq-dev/moq/pull/8756)). `js/net`
polls `getStats()` every second while a route exists, and nothing in JS reads
`RouteInfo.rtt`.

Public API: (a) removes `RouteInfo::rtt` in Rust, JS, and the bindings.
Wire: lite-07-wip ROUTE_START/ROUTE_UPDATE lose the field.

## Waiting

- Maintainer: (a) drop the field and `RouteInfo::rtt` (recommended: no reader, and an unmeasured fan-out); (b) keep it after sweeping publishers against link changes; (c) carry each session's link RTT once per session and let a receiver sum it along the route's path, only for a named consumer
