# [M] @moq/publish file sources and Fanout end cleanly

## Goal

When a capture or file source ends or is cancelled, `@moq/publish` keeps
every frame it owes and leaks none:

- A file track with no samples ends instead of restarting forever without
  yielding to the browser.
- A sample whose conversion fails, or that is waiting to be paced when the
  stream is cancelled, is closed, and cancellation stops the pending wait.
- When a Fanout's source ends cleanly, each reader drains the values it had
  queued before it sees EOF.

## Plan

Decided 2026-11-08 (maintainer, /quest-plan of an external code audit of `9f51fa0966`): milestone ✅ m1 (rejected: m3), audit scores 6, 5 and 5/10.

All three were reproduced at `9f51fa0966`:

- Empty track (`js/publish/src/source/pace.ts:64-73`): when looping is on,
  an exhausted generator restarts at once. `timeline.loops` only checks the
  file's shared duration (`timeline.ts:27-29`, one Timeline per file at
  `file.ts:186`), so a track with no samples spins in microtasks. If a pass
  yields nothing, end that track's stream.
- Leaks (`pace.ts:93-111`): `sample.close()` runs only after `convert` and
  `enqueue` succeed. `cancel()` only calls `samples.return()`, and the wait
  only listens to the effect's signal, so after a cancel, `convert` still
  makes a new VideoFrame or AudioData, and `enqueue` throws. Close the
  sample in `finally`, release the converted value if `enqueue` fails, and
  have `cancel()` abort the wait.
- Fanout tail (`js/publish/src/fanout.ts:143-154`): `#pump` always calls
  `close()`, which empties and releases every reader's queue
  (`:133-138`, `:191-194`). An encoder loses up to `QUEUE` frames when a
  capture or a non-looping file ends. On a clean end, mark readers done and
  let them drain. The owner's `close()`, effect cleanup, cancel and the
  failure path still discard. The existing test (`fanout.test.ts:278-292`)
  passes only because its reader was already waiting; add one where it isn't.

Public API: none. Wire: none.

## Related

- [Publish capture phases](/quest/m1/explicit-state/publish-capture.md) - [moq-dev/moq#8262](https://github.com/moq-dev/moq/pull/8262) reworks capture phases nearby, but touches none of these lines
