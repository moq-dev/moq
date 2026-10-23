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

Public API: none expected. Wire: none.
