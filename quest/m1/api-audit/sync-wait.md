# [S] Sync.wait says what happened

## Goal

`@moq/watch`'s `Sync` stops publishing calls whose correct use depends on
hidden bookkeeping. The decoder-facing calls (`received`, `wait`, `register`)
are `@internal`, and `wait` resolves to whether the frame should be presented
or dropped (reset, skipped, closed, aborted, or unanchored), so no caller
checks `epoch` or `skipped()` around it. The duplicate `out.jitter`, the
dead `probe` input, and the `<moq-watch>` compat shims are gone.

## Plan

Decided 2026-10-22 (API audit; private over a public result type because no
code outside the built-in decoders calls these, and keeping them private
until a consumer needs them is the rule).

- `js/watch/src/sync.ts`: `wait(timestamp, signal?)` captures the epoch
  itself and returns `Promise<boolean>` (`true` presents now); it no longer
  throws when unanchored. Delete `epoch`/`skipped` as caller-facing checks
  and the pre-checks in `js/watch/src/video/decoder.ts`. Reproduced: a wait
  resolves for a frame `skipped()` says not to render.
- `Sync.out.jitter` always equals `delay` since #4162, and `SyncInput.probe`
  is stored but never read. Delete both, the element `jitter` getter
  (`element.ts`), and show one delay readout in
  `js/watch/src/ui/components/latency.ts` and `buffer-control.ts`. Point
  `demo/web/src/viz.ts` and the audio-quality harness
  (`test/audio-quality/clients/js/src/probe.ts`, `schema.ts`, which still
  calls it the network portion) at `out.delay`.
- Delete the `<moq-watch>` compat attributes `reload`, `latency`,
  `latency-min`, `jitter` and the `@internal` `latency`/`latencyMin`/
  `latencyMax` accessors; nothing uses them (a stale comment in moq.pro
  `pronto` mentions one).
- Smaller: `Sync.received`'s debug `label` parameter goes, `Sync.now()`
  returns `Milli`, and `out.instant` is derived from `in.delay` if it stays.

This lands before [A/V clock](/quest/m1/av-clock.md), which reshapes
`SyncInput` into per-track handles; that quest's Plan no longer drops
`probe`.

Public API: `@moq/watch` breaking (`SyncInput.probe`, `out.jitter`, element
attributes, `Sync` methods going internal). Wire: none.
