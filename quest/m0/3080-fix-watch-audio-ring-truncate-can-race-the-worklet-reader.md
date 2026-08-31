# [M] fix(watch): audio ring truncate can race the worklet reader for one quantum

## Goal

Implement and verify the behavior tracked in [#3080](https://github.com/moq-dev/moq/issues/3080)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Follow-up from #3067, which merged with this known and documented.

#### Summary

`SharedRingBuffer.truncate()` (`js/watch/src/audio/shared-ring-buffer.ts`) retreats `WRITE` from the main thread to drop a superseded subscription's write-ahead tail. The AudioWorklet's `read()` runs concurrently and snapshots `WRITE` at the top of the call, so a truncate landing mid-quantum is invisible to the render pass already in flight.

The interleaving, reproduced during review:

1. `read()` loads `WRITE = 3000` with `READ = 2100` and starts copying a 128-sample quantum.
2. `truncate()` retreats `WRITE` to 2200.
3. `read()` finishes, having emitted 128 samples from `[2100, 2228)`  -  the last 28 of which are the stale tail truncate meant to drop  -  and advances `READ` to 2228.

`READ` is now ahead of `WRITE`. The replacement's next `insert()` at 2200 has its first 28 samples trimmed as already played, because they were.

#### Impact

Bounded and self-healing, which is why #3067 shipped without it:

- At most one render quantum of stale audio plays (2.7ms at 48kHz), against the seconds of stale tail #3067 fixes.
- The transient `READ > WRITE` is tolerated everywhere: `read()` returns 0 on a non-positive count, `insert()` restores `WRITE` via `i32Max`, and `resize()` clamps `copyCount` to 0.
- It heals on the first inserted sample past the playhead.

Trimming samples whose slots the worklet already rendered is arguably the correct outcome, since that audio cannot be unplayed. The defect is the stale quantum that reaches the speakers, not the trim.

#### Possible fix

Give the ring an epoch (a new control slot) that `truncate()` bumps. `read()` samples it before and after copying and discards the quantum if it changed, so an invalidated snapshot never reaches the output. That trades a discarded quantum (silence) for a stale one, which is the better failure. Serializing truncation onto the worklet instead would also close it, at the cost of moving the truncate off the main thread and into the message path, which the `SharedArrayBuffer` transport otherwise avoids entirely.

The `postMessage` transport (`AudioRingBuffer`) has no such race: `truncate` and `write` are both ordered on the worklet's message queue.

Whichever way it goes, it wants a deterministic interleaving test that drives `truncate()` between `read()`'s `WRITE` snapshot and its `READ` advance. The current tests are single-threaded and cannot reach it.

## Closes

- [#3080](https://github.com/moq-dev/moq/issues/3080) - close this issue when the quest finishes
