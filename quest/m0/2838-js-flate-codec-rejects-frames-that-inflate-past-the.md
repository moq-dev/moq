# [XS] js/flate: the inflate-cap test times out under parallel load

## Goal

`@moq/flate`'s cap test runs in milliseconds and still proves the decoder
bounds output as it inflates, so `just test` no longer fails on a loaded
machine.

## Plan

`js/flate/src/index.test.ts` builds a 64 MiB decompression bomb
(`"a".repeat(64 * 1024 * 1024 + 1)`) to trip `DEFAULT_MAX_FRAME_SIZE`. The
string allocation, UTF-8 encode, and deflate are pure CPU, so the test takes
2 s idle and 5.3 s under a full `just test`, against bun's 5 s default timeout.

The assertion is about the cap being enforced, which the decoder does as
output is produced, so the value of the cap is irrelevant to the code path.
Exercise it through the existing `maxFrameSize` option with a small cap and a
payload just past it, and pin the default separately: assert
`DEFAULT_MAX_FRAME_SIZE` is 64 MiB and that a decoder built without the option
reports that value as its cap, so the default wiring is tested rather than
only the exported constant. That keeps the test honest and fast rather
than tolerating a slow one with a wider timeout.

## Closes

- [#2838](https://github.com/moq-dev/moq/issues/2838) - close this issue when the quest finishes
