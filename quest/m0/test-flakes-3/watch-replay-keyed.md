# [S] @moq/watch keyed-by-timestamp replay test times out under load

## Goal

`js/watch/src/audio/replay.test.ts`'s `replay > waits out only the keys around
missing media when groups are keyed by timestamp` passes every loaded
`just check`, fixed at its cause.

## Plan

Found on [moq-dev/moq#9521](https://github.com/moq-dev/moq/pull/9521): it timed
out at bun's 5 s default under a full `just check` and passed alone in about
1 s. The test replays a simulated 10 s trace four times, rendering every
128-sample quantum, so its cost is likely CPU rather than waiting: under load the four
replays outrun the budget.

Measure where a replay spends its time first. Recommended: make the replay
itself cheaper, such as skipping the render of quanta no assertion reads, or
cover the same keys with a shorter trace, since the property (only the keys
around missing media are owed) doesn't need 10 s of audio. Don't raise the
timeout.
