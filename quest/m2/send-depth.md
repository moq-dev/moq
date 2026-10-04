# [M] moq-net futures prove Send without a raised recursion limit

## Goal

A downstream crate that requires `Send` on a future awaiting
`announce::Consumer::next()`, or a track or group read, compiles without
nightly's `recursion_depth_exceeding_limit` future-incompat warning and
without raising its own `recursion_limit`.

## Plan

The depth comes from the type graph behind `kio::Shared<OriginState>`
(`rs/moq-net/src/model/origin.rs`): the recursive `RouteNode` children map,
the serve state, and the broadcast and track chain behind remote fronts.
moq-net itself already needs `#![recursion_limit = "256"]` (#4746), which
downstream crates do not inherit.

Decided (2026-10-04): m2, since it is a nightly warning today. Cut the deep
branch (for example flatten the route tree into a map keyed by path) until a
const assertion that these futures are `Send`, modelled on the one in
`session.rs`, compiles at the default limit. Done when the `recursion_limit`
line is deleted. Revisit the milestone if the lint becomes a hard error on
stable.

## Closes

- [#4648](https://github.com/moq-dev/moq/issues/4648) - close this issue when the quest finishes
