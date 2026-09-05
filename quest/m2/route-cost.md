# [S] Route cost in the JS origin

## Goal

The `@moq/net` origin serves a path through the best route it knows, ranked by
cost and then hop count the way Rust's origin does, instead of the newest
announce. The lite-06 route cost that arrives on every announce is read rather
than dropped.

## Plan

`announce.ts` already decodes `Cost { warm, cold }` and `hop.ts` already
carries the hop chain; neither reaches `OriginState.remote`, which keeps
providers newest-first. Carry both on the provider, rank with the same order
as `route_order` in `rs/moq-net/src/model/origin.rs` (cost, chain length, a
deterministic tiebreak), including `Cost::UNKNOWN` for an announce that
carries no cost (free to reach, cold path at the ceiling, so hop count decides
as it did before route cost existed), and re-pick when the chosen route is
retracted.

Tests in `js/net` with the mock transport pair: two sessions announcing the
same path at different costs, the cheaper one serves, its retraction moves
the consumer to the other.

## Related

- [Web P2P](/quest/m3/p2p/README.md) - the watcher-side route pick that line needs
