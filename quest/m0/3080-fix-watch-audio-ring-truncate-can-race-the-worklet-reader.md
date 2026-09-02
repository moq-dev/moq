# [S] watch: an audio ring truncate can race the worklet reader for one quantum

## Goal

A render quantum whose samples a concurrent `truncate()` dropped never reaches
the speakers: the worklet plays silence for that quantum instead of the stale
tail.

## Plan

`SharedRingBuffer` in `js/watch/src/audio/shared-ring-buffer.ts` packs an
epoch and the read cursor into one `BigInt64Array` word, and `read()` publishes
its advance with a compare-exchange on that word, zero-filling and returning 0
when the CAS fails. That is the mechanism this needs, but `truncate()` does not
use it: it CASes only the `WRITE` control slot, so a `read()` that snapshotted
`WRITE` before the retreat copies up to a quantum of the dropped tail, and its
own CAS still succeeds because the epoch did not change. Only the writer's
anchor path bumps the epoch today.

- `truncate()` bumps the epoch in the packed word alongside retreating `WRITE`,
  ordered against `read()`'s loads so any read that saw the old `WRITE` finds
  the epoch changed at commit time. Its CAS then fails, the quantum is
  zero-filled, and the trim of already-rendered slots stays as it is (that
  audio cannot be unplayed).
- The `postMessage` transport (`AudioRingBuffer`) has no such race, since its
  truncate and write are ordered on the worklet's queue.
- Test: a deterministic interleaving that drives `truncate()` between
  `read()`'s snapshot and its commit and asserts zero output for that quantum,
  with the output buffers pre-filled with nonzero samples and checked at full
  length, since the current helper slices to `samplesRead` and would pass on
  an empty slice.
  The current tests are single-threaded and cannot reach it, so split
  `read()`'s snapshot and commit into steps a test can interleave.

## Closes

- [#3080](https://github.com/moq-dev/moq/issues/3080) - close this issue when the quest finishes
