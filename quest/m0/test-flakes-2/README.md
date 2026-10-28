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
  (one cancel race was a retry sweep jittered past a fixed settle
  budget). Wait out the jitter's whole window, not a count of small steps.

This README's own work, after the children: run `just check --all` several
times on a loaded machine, as the first round did.

The redundant failover drift moved to m0 on 2026-10-20 (maintainer) and is
fixed by [moq-dev/moq#6113](https://github.com/moq-dev/moq/pull/6113).

Added 2026-10-21 and 2026-10-22 (maintainer): failures from the m0/m1
merge runs, one child each.

Decided 2026-10-26 (maintainer): the epic moved from m1 to m0, since every
flake now fails a whole batch of the [batched merge gate](/quest/m0/ci/land.md)
rather than one PR. The queue's recurring flakes from 2026-10-23 to 10-26
were already fixed (the moq-rtsp stall tests, the moq-gst pinned-instance and
sibling-restart tests, the moq-mux missing-sequence test,
`js/common/worklet.test.ts`, and the IETF datagram range test in
[moq-dev/moq#6593](https://github.com/moq-dev/moq/pull/6593)).

Public API: none. Wire: none.

## Required

- [FFI publisher audio gaps](/quest/m0/test-flakes-2/ffi-audio-gap.md) - interop tone checks stop finding gaps from FFI publishers
- [IETF22 fallback](/quest/m0/test-flakes-2/ietf22-fallback.md) - an IETF22 browser viewer follows a fallback within the media lane's bound, keeping its session and its audio
- [Swapped Rust to JS Opus](/quest/m0/test-flakes-2/swapped-js-opus.md) - the `rust-swapped -> js` interop cell stops timing out under load
- [JS host-task yields follow the clock](/quest/m0/test-flakes-2/js-host-yields.md) - `Budget` and `nextTask` yield through the injected clock, so `Manual` tests stop racing them
