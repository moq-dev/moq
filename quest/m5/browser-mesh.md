# [L] Browser tabs forward for their peers on a local network

## Goal

A page that opts in forwards broadcasts between the peers it holds direct
sessions with, so browsers on one local network form a mesh: a broadcast one
tab publishes reaches a tab it has no direct session with through a tab that
has both, and keeps flowing when the site's internet drops. A tab is then a
routing node like any relay, and the relay session stays the fallback route.

## Plan

Decided 2026-11-10 (maintainer, /quest-plan on lite-07 routing): "Web clients
might forward too, honestly it would be lit to have P2P meshes for browsers
on the local network so please plan that."

- ✅ The forwarding engine is the Rust router, run in the tab through the
  [Rust-to-TypeScript port](/quest/m5/rs2ts-js.md) (rejected: handwriting
  the forwarding rule in `js/net`, which was feasibility, route requests,
  holds, and echoes when this was decided and is paths, withdrawals, holds,
  and the doubt since the
  [routes epic](/quest/m2/cluster-routes/README.md)'s Plan item 27, a second
  implementation of the part of the protocol that has produced the most
  bugs; pulling the port forward to m4). So the tab runs the router the route
  model checks, and the [P2P epic](/quest/m4/p2p/README.md)'s "no second
  routing implementation" holds.
- ✅ m5, behind that port.

This lifts three statements once it lands: the P2P epic's "A browser never
carries another peer's traffic", the
[cluster routing](/quest/m2/cluster-routing/README.md) plan's browser tabs
keeping `proxy` off, and `js/net`'s non-transit
tab. Until then each stays true.

It also settles why every publisher stays an origin node (Plan item 18 of
[Routes and announces](/quest/m2/cluster-routes/README.md)): a tab
that forwards needs its own identity.

To settle when this starts:

- Which sessions a tab meshes over. The P2P epic's data channels pair on a
  LAN with host and mDNS candidates and an empty ICE server list; a tab can
  also dial a local relay by WebTransport with a pinned certificate hash.
  Recommended: reuse the P2P epic's `Peers`, with `proxy` turned on per
  connection, rather than a second connection type.
- Trust. A forwarding tab can alter what it forwards, and a scoped peer's
  identities are qualified, so a second scoped hop is refused today. Say
  which grant LAN peers give each other, and whether the mesh needs more
  than one scoped hop.
- Limits a tab needs that a relay does not: how many peers it forwards for,
  and what it does on battery or a metered link.

Test in the browser suite: three tabs in a line, where the ends share no
session, deliver a broadcast end to end, reroute when the middle tab closes
and a relay path exists, and never loop.

Public API: a per-connection forwarding option in `@moq/net` or `@moq/p2p`.
Wire: none expected; a forwarding tab speaks the lite-07 route layer as a
relay does.

## Required

- [Ship JS-only @moq/net from Rust](/quest/m5/rs2ts-js.md) - the Rust router and model running in the tab
- [P2P](/quest/m4/p2p/README.md) - direct browser sessions, their signaling, and `Peers`
