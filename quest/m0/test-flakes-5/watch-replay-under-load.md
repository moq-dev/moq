# [S] Audio replay tests finish within their timeout under load

## Goal

`js/watch/src/audio/replay.test.ts`'s minute-long replays, including `replay against the shared
clock > keeps the audio a fixed target short of the spread receives late, where a zero stretch
skips it` and `replay at auto > sizes the target from the arrival spread, not a round trip`, pass
every run, alone and in a loaded full `just check`, fixed at their cause.

## Plan

Seen on 2026-11-27 in a full local `just check` on
[moq-dev/moq#9712](https://github.com/moq-dev/moq/pull/9712), which changes no JS: both failed at
bun's default 5 s timeout (5005 ms and 5013 ms). Alone, `bun test src/` in `js/watch` passed
656 of 656.

Each test replays 60 s of 20 ms frames, some twice, entirely in CPU, so a loaded host runs past the
timeout. Measure what one replay costs and why: the test itself may do needless work per quantum,
such as re-sorting or copying the trace, or `replay` may. Cut that cost, or shorten the trace to
the least that still exercises the estimator's settled state, rather than raising the timeout.
