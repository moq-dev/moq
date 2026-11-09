# [L] js/net route layer

## Goal

`js/net` encodes, decodes, and resolves lite-07's route layer: it reads
ROUTE and ANNOUNCE_FLOOR, merges announcements by floor as the contract
says, echoes every ROUTE_END, and pins each request to the identity it
selected. Browsers stay non-transit: a tab advertises its own identity and
publications only, with `proxy` off, until the
[Rust-to-TypeScript](/quest/m5/rs2ts-js.md) port; P2P reuses this model
rather than adding a browser forwarding engine.

## Plan

Decided 2026-10-27 (maintainer), from
[moq-dev/moq#6947](https://github.com/moq-dev/moq/pull/6947): in `js/net`'s
`origin.ts` a detached request stays on its own route instead of following
the cheapest one; fix it with the route-selection rework here.
- Feed the availability API from [Announcement availability](/quest/m2/cluster-routing/routes/availability.md).
- Rust/JS interop covers the route messages, the floor marker, and the echo.
- Decode the temporary lite-06 compatibility chain that lite-07 announcements carry ([mixed-version ring](/quest/m1/mixed-version-ring.md)). JS translates nothing, so it keeps no chain.
- Restore the native JS lite-07 lanes. Decided 2026-11-06 (maintainer,
  /quest-complete on [moq-dev/moq#8023](https://github.com/moq-dev/moq/pull/8023)),
  asked how to handle the in-tree lanes running JS on lite-07-wip once the
  Rust side moved to the route wire: ✅ **skip them until this quest lands**,
  so the nightly report stays meaningful. Rejected: leave them red; hold
  #8023 for this quest. Every skip names this quest (`grep -rn routes/js.md
  test rs`): `JS_LITE07_SKIP` in `test/interop/interop.sh` and `socket.sh`
  (native JS grant printing, claims, track replacement, sockets, the browser
  route key proof, the lite-07 compress and drop legs, the compressed catalog's lite-07 reader, and
  `--versions moq-lite-07-wip` with JS clients), `lite_tracks_interop` in
  `test/justfile`, and the lite-07 legs of `max_age_relay_javascript` and
  `relay_fill_javascript`.

## Required

- [Announcement availability](/quest/m2/cluster-routing/routes/availability.md) - the API this feeds
- [Mixed-version ring](/quest/m1/mixed-version-ring.md) - defines the compatibility chain field this decodes

## Related

- [Routes and announces](/quest/m2/cluster-routing/routes/README.md) - the contract
