# [M] The browser publisher and player report stats

## Goal

`@moq/stats` mirrors `moq-stats`: the frame schemas, `Producer<E>`,
`Consumer<E>`, the aggregate, and the `.stats` naming. `@moq/hang` defines the
media stats schema, `<moq-watch>` publishes subscriber stats and
`<moq-publish>` publisher stats when given a stats path, and the demo
dashboard reads relay stats through the package instead of its own copies.

## Plan

- `js/stats`: zod schemas for `Traffic`, `Presence`, and `Stats<E>` with the
  extension flattened; snapshot-mode producer pairs (`.json` and `.z`) with
  the same delta and compression settings as Rust; the per-broadcast track
  served on request through the broadcast's requested-track handle; a
  consumer keyed by tier, role, and path; and the aggregate. Fixtures are the
  Rust crate's, shared through `js/test`, so the two encoders stay wire
  identical.
- `@moq/hang` adds `Stats` in `js/hang/src/stats.ts`, mirroring
  `rs/hang/src/stats.rs` field for field.
- `js/watch`: the decoder `Stats` signals grow the counters the schema needs
  (late, stalled duration, underruns from the worklet's count, decode errors,
  newest timestamp) and a `stats` attribute names the broadcast to publish
  at; `js/publish` does the same for the encoder counters, with the
  connection's `WebTransport.getStats()` as the transport section. The
  existing UI stats panels read the same signals.
- `demo/web/src/stats.ts` drops its hand-written interfaces and
  `STATS_PREFIX` for the package, and the demo gains a stats path on both
  elements so the media smoke test reads a browser viewer's report through
  `moq export stats`.
- `doc/lib/js` documents the package.

## Required

- [Schema and library](/quest/m2/qos/stats/schema.md) - the wire shape this
  mirrors

## Closes

- [#2735](https://github.com/moq-dev/moq/issues/2735) - close this issue when the quest finishes
