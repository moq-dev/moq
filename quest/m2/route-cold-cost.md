# [S] The route cold cost crosses moq-ffi intact

## Goal

An application that observes a route through the bindings and announces it
again reproduces the route it saw. `MoqRoute` carries both halves of
`origin::Cost { warm, cold }`, so a truthful `{warm: 0, cold: N}` is never
rewritten to `{warm: 0, cold: 0}`, the publisher's own cost, on its way back
through `dynamic(prefix, route)` or `broadcast.announce(route)`. A publisher
seeding only a production cost still sets one number.

## Plan

On dev `rs/moq-ffi/src/origin.rs` maps the pair onto one scalar in each
direction: `From<origin::Route>` reports `cost.warm` and drops `cold` (:42),
while `TryFrom<MoqRoute>` calls `with_cost(u64)` (:51), which sets both halves
(`Cost::new`, rs/moq-net/src/model/origin.rs:449). `route_order` ranks warm
first and breaks ties on cold (origin.rs:633-639; `Cost` derives `Ord` over
`(warm, cold)` at :424; drafts/draft-lcurley-moq-lite.md:409), so an
understated cold wins ties it should lose.

- `MoqRoute` gains `cold: Option<u64>` with a uniffi default of `None`, meaning
  "same as `cost`" when omitted, which is right for a publisher seeding a
  production cost. `cost` stays the warm half under its current name. Both
  conversions become lossless: `From` fills both fields, `TryFrom` builds
  `Cost { warm: cost, cold: cold.unwrap_or(cost) }`.
- Additive in every generated binding. `rs/libmoq` now exposes `moq_route`
  (hops plus one cost); add `cold` there too and regenerate `moq.h`. The
  wrappers describe a route as hops and cost and
  gain the field there: py/moq-rs/moq/origin.py:51,72,258;
  go/wrapper/origin.go:72,189 and go/wrapper/types.go:41;
  swift/Sources/Moq/Origin.swift:60,208;
  kt/moq/src/jvmAndAndroidMain/kotlin/dev/moq/Aliases.kt:124. The
  `doc/lib/{py,swift,kt,go}/index.md` pages do not mention cost today, so the
  field is added wherever each shows a route. Dart already has the announce
  API, so the field lands there with the other wrappers.
- Tests: a route observed through the announcement stream and announced again
  compares equal including cold; an omitted cold equals warm.

Branch from dev.

## Closes

- [#2933](https://github.com/moq-dev/moq/issues/2933) - close this issue when the quest finishes
