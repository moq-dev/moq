# [S] moq-transcode's live multi-rung test survives load

## Goal

`moq-transcode tests::live_multi_rung` (`rs/moq-transcode/src/lib.rs`) passes
reliably in a loaded merge-queue `just ci`, fixed at its cause, never by
raising a timeout or adding a retry. Done when the test no longer fails the
queue and a loaded local repeat of it passes.

## Plan

Planned 2026-11-03 (maintainer, `/quest-plan`). It failed two merge-queue
batches on 2026-11-03, both with the same panic:

- [Run 41599906715](https://github.com/moq-dev/moq/actions/runs/41599906715),
  the batch for [moq-dev/moq#7544](https://github.com/moq-dev/moq/pull/7544),
  at 08:03Z after 0.909 s.
- [Run 41666527065](https://github.com/moq-dev/moq/actions/runs/41666527065),
  the batch for [moq-dev/moq#7692](https://github.com/moq-dev/moq/pull/7692),
  at 15:44Z after 1.566 s.

```text
FAIL [   1.566s] (9500/9597) moq-transcode tests::live_multi_rung
thread 'tests::live_multi_rung' (26884) panicked at rs/moq-transcode/src/lib.rs:550:52:
called `Result::unwrap()` on an `Err` value: Cancel
```

Line 550 is the `group.read_frame().await.unwrap()` that drains each rung's
first group, so a rung's output group was cancelled before it held all five
source frames.

- The test runs in real time on purpose: its comment says a paused clock let
  the source outrun the feed's bounded broadcast while the rungs encoded on
  their own threads. Under load, real time can do the same thing. Find what
  cancels the rung's group: the feed's lag, a superseding group, or the rung
  restarting its encoder.
- Follow the epic's rules: drive the source from an event the rungs report,
  or from a `kio::clock::Manual` clock, rather than a wall-clock pace. If the
  code under test drops a group it should finish, fix the code.
- Reproduce it under load first, for example beside a full `cargo nextest`.

Follow the rules in [More tests under load](/quest/m0/test-flakes-2/README.md).
