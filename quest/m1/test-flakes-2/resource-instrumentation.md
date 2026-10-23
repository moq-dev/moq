# [S] The media test's resource instrumentation reports what the player holds

## Goal

The media interop test's resource instrumentation check passes reliably under
a loaded `just check`, fixed at its cause. It sometimes fails with the player
reporting nothing to release while it is still playing.

## Plan

Seen twice locally while working
[moq-dev/moq#6302](https://github.com/moq-dev/moq/pull/6302), once with the
fixture's mono pin and once without, so the pin is unrelated. Reproduce under
load with the diagnostics #6302 added (a failed media window prints each page's
long animation frames and the host's busiest processes), then decide whether
the player drops its resource handles early or the check samples before they
exist. Never widen the check's wait to hide it.

Investigated on 2026-10-22 at
[fd59eb18a5](https://github.com/moq-dev/moq/commit/fd59eb18a5): one normal
detach-only run and one run of pause, disable, rejoin, and detach restricted to
two available CPUs passed. A full run on those two CPUs passed the positive
lifecycle and the leaked-session negative control; an unconstrained full run
also passed that control. These five checks saw ownership before detach and
released it correctly. The three checks with the added diagnostic each reported
one transport and one AudioContext before detach, with no socket or worker.
This did not reproduce the original failure or distinguish early release from
sampling before ownership exists.

The draft checkpoint logs the successful ownership sample and, on an
instrumentation failure, the complete sampled player state plus the existing
bounded long-animation-frame diagnostics for both pages. It does not change
the predicate, waits, or production instrumentation. Capture a failing run with
this evidence, then fix the causal ownership or sampling transition and cover
that transition deterministically. The goal remains open.

Both full runs failed separately on audio-tone agreement before reaching the
intended check in one of their cases. That recurrence belongs to
[Media tone windows](/quest/m1/test-flakes-2/media-tone-windows.md), not evidence
that resource instrumentation failed.

Public API: none expected. Wire: none.
