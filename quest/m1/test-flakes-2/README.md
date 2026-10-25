# More tests hold up under load

## Goal

A second round after [#4286](https://github.com/moq-dev/moq/pull/4286):
tests that pass alone but have failed under a loaded `just check`, or
intermittently in CI or interop, pass reliably, each fixed at its cause,
never by raising a timeout or adding a retry.

## Plan

Decided in the 2026-09-30 audit: the round held independent flakes in one
quest, so it split into one child per flake, grouping only those that share
a fixture. Each child lands on its own.

Rules every child keeps:

- Prefer a paused clock over wall time (`moq-cli`'s subscribe tests already
  use `#[tokio::test(start_paused = true)]`), or assert on an event instead
  of a deadline. If a test is slow under load because the code under test is
  slow, fix that.
- A paused clock auto-advances while the runtime idles, so it can fire a
  timer before a real socket delivers. Where real sockets fight the paused
  clock, drive the timers with a `moq_clock::Manual` clock or the sim (every
  crate's timers run on `moq-clock`), or move the test off real sockets.
- A mocked clock still flakes when the code jitters its timers with `rand`
  (the draft-16 cancel race was a retry sweep jittered past a fixed settle
  budget). Wait out the jitter's whole window, not a count of small steps.

This README's own work, after the children: run `just check --all` several
times on a loaded machine, as the first round did.

The redundant failover drift moved to m0 on 2026-10-20 (maintainer) and is
fixed by [moq-dev/moq#6113](https://github.com/moq-dev/moq/pull/6113).

Added 2026-10-21 and 2026-10-22 (maintainer): failures from the m0/m1
merge runs, one child each.

Public API: none. Wire: none.

## Required

- [Media resource instrumentation](/quest/m1/test-flakes-2/resource-instrumentation.md) - the media test stops seeing a playing player report nothing to release
- [Tone check grace](/quest/m1/test-flakes-2/tone-grace.md) - the interop tone check charges its grace only for audible playback, so a refilling ring under CPU starvation is not a failure
- [IETF22 fallback](/quest/m1/test-flakes-2/ietf22-fallback.md) - an IETF22 browser viewer follows a fallback within the media lane's bound, keeping its session and its audio
- [macOS receive credit](/quest/m1/test-flakes-2/macos-receive-credit.md) - `partial_control_requests_give_way` passes on the macOS lane, after checking for a shared platform cause
- [moq-mux missing sequence](/quest/m1/test-flakes-2/mux-missing-sequence.md) - `missing_sequence_skips_on_live_track` runs on a paused clock
- [moq-rtsp stall timers](/quest/m1/test-flakes-2/rtsp-stall-timers.md) - the 200 ms stall and keyframe tests run on a mocked clock
- [moq-gst sibling resume](/quest/m1/test-flakes-2/gst-sibling-resume.md) - an idle pad reads its named epoch again after a sibling restart, a likely liveness bug fixed before the test
- [moq-gst pinned replay](/quest/m1/test-flakes-2/gst-pinned-replay.md) - the pinned-instance finish test waits on events instead of sleep loops
- [Dart leak RSS](/quest/m1/test-flakes-2/dart-leak-rss.md) - the leak tests measure what moq owns, not process RSS
