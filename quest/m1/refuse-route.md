# [M] A relay that refuses a forwarded route drops the route, not the session

## Goal

A lite-07 relay that refuses a route because of its content, which the
neighbour only forwarded, withdraws that route and keeps the session. Such
content includes the path, its hop count, the flags, and the claims under the
route. The neighbour's other routes and claims stay live. Only a fault in the
neighbour's own framing or per-session state closes the session, such as a
Route ID it never opened or a message out of order on its stream. A relay
that cannot encode a route for a peer, such as one past that peer's hop
limit, does not forward it, and the session stays open.

This matters for moq.pro, which rolls out with relays on the newest and the
previous release side by side. A newer release that adds a check, raises a
limit, or sets a new flag would otherwise make older relays close sessions to
neighbours that did nothing wrong, and the closes cascade across the mesh. BGP
hit the same problem and answered with "treat as withdraw"
([RFC 7606](https://www.rfc-editor.org/rfc/rfc7606)).

## Plan

Decided 2026-11-13 (maintainer, /quest-plan on the BGP failure modes):

- ✅ Drop the route and keep the session; close only on the neighbour's own
  framing or state (rejected: closing as today and relying on a newer relay
  to send nothing an older peer's version lacks, which misses a newer relay
  adding a check against content older relays forward; skipping unknown
  flags and fields and keeping the route, which accepts content the relay
  cannot interpret).
- ✅ m1 (rejected: m2 beside the switch). It lands first on today's lite-07
  codec, so it waits on nothing in m2, and
  [the switch](/quest/m2/cluster-routes/switch.md) keeps the rule when it
  rewrites the codec.
- ✅ It gates the lite-07 cut
  ([lite07-finalize](/quest/m2/lite07-finalize.md) requires it), since once
  lite-07 is published, changing how it reacts to a refused route needs
  lite-08 (rejected: shipping lite-07 without it).
- ✅ Test within one binary (rejected: a nightly job that builds the
  `release` relay and runs it in a mesh with `main` relays, a second build
  for a check the fixture already covers by injecting refusals).

Today `decode_advert` in `rs/moq-net/src/lite/announce.rs` fails the decode,
and so closes the session, on an unknown flag bit or more than 32 hops, and
`encode_advert` errors on a route it cannot encode. Sort each check in
`lite/announce.rs` and `lite/subscriber/routed.rs` into content the
neighbour forwarded or state the neighbour owns. A refusal must still be
loud: log it with the route and the reason, and count it, so a refused route
is never silently missing.

Decide in the PR whether an unknown flag bit refuses the route or is
ignored, given that the flags byte is the obvious place for a newer release
to extend a route. Mirror the rule in `js/net` and in the draft's error
handling for route messages.

Fixtures on real sessions, with mocked time: a neighbour forwards a route
that the relay refuses, once for each refusal the codec and router make,
and the session, the neighbour's other routes, and their claims stay live
while the refused route's claims withdraw; a route past the next peer's hop
limit is not forwarded, and that session stays open; an unknown Route ID
still closes the session.

## Related

- [Routes and announces](/quest/m2/cluster-routes/README.md) - the route messages this rule covers
- [A dual-homed publisher stays reachable while one link flaps](/quest/m2/cluster-routes/flapping-publisher.md) - another failure mode from the same BGP comparison
- [A request loop heals once routes settle](/quest/m2/cluster-routes/request-loop.md) - another failure mode from the same BGP comparison
