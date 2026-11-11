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
- Feed the availability API (`RouteInfo.available` and the `offline` route kind) from the route layer.
- Rust/JS interop covers the route messages, the floor marker, and the echo.
- Decode the hop list on ROUTE_START and ROUTE_UPDATE and send an empty one for the tab's own route, once [the fix](/quest/m2/cluster-routing/routes/echo-order.md) adds it. A tab forwards nothing, so it never appends itself; the [browser mesh](/quest/m5/browser-mesh.md) does.
- Encode lite-07's Route ID (ANNOUNCE_START) and Node ID (every TRACK, TRACKS, SUBSCRIBE, FETCH, and DATAGRAMS request, 0 when the publisher selects the node) as Rust does, and pin `js/net/src/lite/announce.test.ts`'s goldens to `lite::compress`'s `GOLDEN` and `JS_LITERAL`.
- Leave the temporary Lite-06 Chain Setup Parameter unsent: JS translates nothing, so it keeps no chain, and a Rust peer sends the chain only to a peer that offers it.
- Restore the native JS lite-07 lanes. Decided 2026-11-06 (maintainer,
  /quest-complete on [moq-dev/moq#8023](https://github.com/moq-dev/moq/pull/8023)),
  asked how to handle the in-tree lanes running JS on lite-07-wip once the
  Rust side moved to the route wire: ✅ **skip them until this quest lands**,
  so the nightly report stays meaningful. Rejected: leave them red; hold
  #8023 for this quest. Every skip names this quest (`rg routes/js.md
  test rs .github sh/gh`): `JS_LITE07_SKIP` in `test/interop/interop.sh` and `socket.sh`
  (native JS grant printing, claims, track replacement, sockets, the browser
  route key proof, the lite-07 compress and drop legs, the compressed catalog's lite-07 reader, and
  `--versions moq-lite-07-wip` with JS clients), `lite_tracks_interop` in
  `test/justfile`, and the lite-07 legs of `max_age_relay_javascript` and
  `relay_fill_javascript`. Also restore `test drain` in the nightly matrix,
  restore the `Datagram tracks` interop workflow step and its report outcome, reset
  `datagram_tracks_javascript`'s ignore reason, remove its test-inventory entries,
  and remove the default media `replacement`
  route-wire quarantine (every replacement publisher pins lite-07 even for
  older readers). Keep the separate Firefox audio-tone quarantine until
  [moq-dev/moq#8058](https://github.com/moq-dev/moq/pull/8058) resolves it.
  The local drain/datagram recipes and `just test media --media-cases replacement` remain
  available to validate the port.
- Decided 2026-11-10 (maintainer, /quest-plan on lite-07 routing): every
  publisher stays an origin node, a tab included, so this port keeps the
  origin side (its own Node ID, seqno, and answering ROUTE_REQUEST). A tab
  that forwards is planned separately, on the Rust router:
  [browser mesh](/quest/m5/browser-mesh.md).
- The Announce Stream becomes one one-way stream per side with a set of
  prefixes ([the route stream](/quest/m2/cluster-routing/routes/route-stream.md)).
  Whichever of the two lands second adapts: JS then shares one stream across
  its `announced()` calls, with the covering-prefix logic Rust's `Discovery`
  has, instead of one stream per call.

## Related

- [Routes and announces](/quest/m2/cluster-routing/routes/README.md) - the contract
