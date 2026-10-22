# [S] Watch and publish trim

## Goal

`@moq/watch`, `@moq/publish`, and `@moq/room` users can name every type
their public signatures take without a new direct dependency, unused watch
outputs are gone, and state a component derives is read-only to callers.

## Plan

Decided 2026-10-22 (API audit):

- Watch, publish, and room types now take `Milli`/`Nano` from `@moq/clock`
  and `Net.Time.Milli` is gone, but none re-exports the clock. moq.dev calls
  `Net.Time.Milli` through `@moq/watch`. Add `export * as Clock from
  "@moq/clock"` to each entrypoint (js/AGENTS.md re-exports dependencies
  under namespaces) and MIGRATE entries in watch and publish.
- Delete `Video.Decoder.arrivals()` and its producer plumbing; nothing uses
  it. Keep `Audio.Decoder.arrivals()` (moq.dev's voice buffers wait on it)
  and document that an open handle must be read or closed, since each
  observer queues every event.
- Delete `Player.error` and the element `error` getter (nothing reads them),
  and make `out.error` types consistent across sources, decoders, and the
  renderer.
- `Audio.Decoder.out.underruns` moves into `Stats.underruns`, as
  [JS stats](/quest/m2/stats/js.md) plans.
- `Publish.Broadcast.net` (`js/publish/src/broadcast.ts`) is a writable
  `Signal` (inherited); expose a `Getter` backed by private state.
- Small: mark `Watch.Broadcast.relativeBroadcast` `@internal`; make the
  `fade` knobs private constants; trim publish `parseBackend` like watch's;
  publish support reports `aac` as a `Level` like watch.

Public API: `@moq/watch`, `@moq/publish`, `@moq/room` (additive re-exports,
removals). Wire: none.
