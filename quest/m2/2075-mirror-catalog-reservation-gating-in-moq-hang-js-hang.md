# [M] Mirror catalog reservation gating in @moq/publish (js/publish)

## Goal

A browser publisher's first catalog snapshot is complete: renditions declared
in separate ticks (video, then audio) never emit a partial catalog that a
one-shot consumer (fMP4, MPEG-TS) locks onto. Wire and catalog schema are
unchanged; the gate is publisher-side timing only.

## Plan

Rust already gates: `catalog::Producer::reserve()` returns a `Reserved`
(`rs/moq-mux/src/catalog/producer.rs:360`), each rendition holds a clone until
its config is set, and the catalog is withheld until the last one drops, so
exactly one complete snapshot publishes (#2072, on main).

The JS target is `CatalogProducer` in `js/publish/src/catalog.ts:15-46`:
`mutate` (`:20-25`) pushes every edit to every subscriber at once, and `serve`
(`:33-46`) seeds a new subscriber with the current value. The gate point is
the catalog rebuild in `js/publish/src/broadcast.ts:171-195`, which writes
whatever renditions have resolved so far.

Settle the design, then implement:

- TS shape: `reserve()` returning a handle released when the rendition's config
  is set or its effect cleans up, an options flag, or an explicit
  `complete()`. The `signals`/`Effect` lifecycle may suggest its own idiom.
- Withhold the initial publish until every reservation resolves, then publish
  incrementally; producers that never reserve keep publishing incrementally.
- Whether JS needs the shared gate across several importers, or only
  complete-first-snapshot for one producer.

Additive on `@moq/publish`, so it lands on main. If the chosen shape changes
`mutate` or `serve`, it is breaking and returns to m1 on dev.
`demo/web/src/publish.ts:418` drives `catalog.mutate` directly and follows any
change.

## Closes

- [#2075](https://github.com/moq-dev/moq/issues/2075) - close this issue when the quest finishes
