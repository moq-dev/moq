# [S] Signals and the encoded preview keep their promises

## Goal

`Computed.close()` resets its value as `js/AGENTS.md` and its doc promise,
and `<moq-publish preview="encoded">` survives bitrate changes and errors
instead of blanking or stopping for good.

## Plan

Found by the 2026-10-24 pre-cut audit at 151a529670. Source-inferred,
inherited.

- `js/signals/src/index.ts:1193` closes a `Computed` but keeps its last
  value and never notifies, so an outside holder keeps a stale derived value
  (documented otherwise at `:1153`). Set it to undefined after closing, or
  fix the docs if a consumer depends on the current behavior.
- `js/publish/src/preview.ts:231-264` rebuilds its encoder and decoder on any
  `encoder.out.resolved` change, which includes the grant bitrate sampled
  every 100 ms, so the preview blanks until the next keyframe; any error
  closes it permanently. Key the rebuild on codec and size, apply bitrate
  with `configure()`, and recover on the next change after an error.

Public API: none. Wire: none.
