# [L] js/net route layer

## Goal

`js/net` encodes, decodes, and resolves lite-07's route layer: it reads
ROUTE and ANNOUNCE_FLOOR, merges announcements by floor as the contract
says, echoes every ROUTE_END, and pins each request to the identity it
selected. Browsers stay non-transit: a tab advertises its own identity and
publications only, with `proxy` off, until the
[Rust-to-TypeScript](/quest/m2/rs2ts-js.md) port; P2P reuses this model
rather than adding a browser forwarding engine.

## Plan

Decided 2026-10-27 (maintainer), from
[moq-dev/moq#6947](https://github.com/moq-dev/moq/pull/6947): in `js/net`'s
`origin.ts` a detached request stays on its own route instead of following
the cheapest one; fix it with the route-selection rework here.
- Feed the availability API from [Announcement availability](/quest/m1/cluster-routing/routes/availability.md).
- Rust/JS interop covers the route messages, the floor list, and the echo.

## Required

- [lite-07 route wire](/quest/m1/cluster-routing/routes/wire.md) - the messages
- [Announcement availability](/quest/m1/cluster-routing/routes/availability.md) - the API this feeds

## Related

- [Routes and announces](/quest/m1/cluster-routing/routes/README.md) - the contract
