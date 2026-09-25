# [L] Rust routing

## Goal

`moq-net` routes lite-07 announcements by source, per-route seqno, and cost,
under the feasibility condition and adjacent-only exclusion that the
[Babel routing](/quest/m0/babel/README.md) line specifies. Pre-07 and IETF
sessions keep hop lists through the bridge.

## Plan

- `rs/moq-net/src/model/origin.rs`: `Route` gains source, seqno, and the
  anonymous flag. `route_order` drops the hop-length and `fnv_key(prefix,
  hops)` tie-breaks. `RouteEntry::visible_to` excludes only the adjacent peer,
  so `Horizon` narrows to that peer. The feasibility table lives beside the
  routes and garbage-collects entries for sources with no live route.
- `rs/moq-net/src/lite/`: the codec for the new ANNOUNCE_START/UPDATE fields
  and ANNOUNCE_REFRESH. The publisher answers or forwards refreshes; the
  subscriber applies feasibility and sends refreshes when starved. It builds on
  the stateful codec from [Announce compression](/quest/m1/announce-compression.md)
  and deletes its hop-tail half.
- Cost Parameter: on lite-07 the link cost is at least 1 by construction.
- Bridge: pre-07 lite and IETF (`ietf/cluster.rs`) sessions map hops to and
  from source routes.
- Consumers: the relay's `/nodes` (`rs/moq-relay/src/nodes.rs`) reports source
  and cost in place of the path. `Route` is exposed through `moq-ffi` and
  `libmoq`, so this is a published API break: retarget to `dev` if `hops` has
  to leave the public type.

Tests: codec round-trips and violations, feasibility accept/refuse, a starved
relay recovering through REFRESH, adjacent exclusion on both advertise and
serve, and anonymous minting.

Benchmark: announce messages and bytes per publish, reroute, and relay loss
on a simulated mesh, swept over relay count and degree, lite-06 against
lite-07.

## Related

- [JS codec](/quest/m0/babel/js.md) - the browser side of the same wire
