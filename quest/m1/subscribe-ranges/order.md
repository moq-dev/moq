# [L] Subscription order

## Goal

A subscriber chooses whether a track serves pending groups oldest or newest
first. `Subscription` carries `order: Order` (`Ascending` or `Descending`,
default `Descending`, today's rule) in Rust and `@moq/net`, and the track's
cursor serves its own subscriber in that order: ascending drains a backlog
oldest first instead of skipping to the newest group. moq-transport SUBSCRIBE
GROUP_ORDER and the lite-07 SUBSCRIBE carry it. Ranges stay in
[Model ranges](/quest/m1/subscribe-ranges/model.md).

## Plan

Split out of the model quest (maintainer, 2026-10-13) so it can land on
moq-dev/moq#5370, which added a JS GROUP_ORDER codec field nothing set or
honored. Build on that contributor's commit and credit them with a
`Co-authored-by:` trailer.

- **Name.** `Order` under the track module (`Order::Ascending`,
  `Order::Descending`; JS `"ascending" | "descending"`), not `GroupOrder`.
  The private IETF `GroupOrder` maps to it; draft-14's 0 (any) decodes as
  `Descending`, so no default variant is representable in the model.
- **Aggregation.** Subscribers sharing one upstream request may want
  different orders; `Descending` wins upstream, so live subscribers stay
  fresh. Each local cursor still serves its own subscriber in its own order
  from the cache.
- **Wire.** lite-07 SUBSCRIBE and SUBSCRIBE_UPDATE carry `order`, documented
  in `drafts/draft-lcurley-moq-lite.md`; lite-07 is unpublished. moq-transport
  maps GROUP_ORDER both ways, and its publisher honors `Ascending`.
- **Older wires.** lite-06 and older cannot carry the field: an `Ascending`
  subscription over them falls back to `Descending` (maintainer, 2026-10-13).
- Cover on mock time, in-process and through a relay, in both languages: a
  backlog served ascending and descending, an ascending and a descending
  subscriber merged on one upstream request, and an `Ascending` request over
  lite-06 served newest first.
