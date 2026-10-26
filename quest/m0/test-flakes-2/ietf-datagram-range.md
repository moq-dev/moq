# [S] IETF datagram range test

## Goal

`@moq/net`'s "IETF %s: datagrams honor the absolute object range"
(`js/net/src/ietf/publisher.test.ts`) passes reliably under a loaded
`just check`, fixed at its cause.

## Plan

It failed once in the merge queue, in
[run 40003622936](https://github.com/moq-dev/moq/actions/runs/40003622936)
for [moq-dev/moq#6528](https://github.com/moq-dev/moq/pull/6528) on
2026-10-23: the received objects were `[]` where `[1]` was expected, after
4 ms, so an assertion ran before the datagram arrived rather than a timeout
firing. It was the only queue failure of this test in about 470 runs from
2026-10-23 to 10-26. Reproduce it under load first; if it cannot be
reproduced, find the ordering the test assumes by reading it, and wait on the
event instead of the assumption.

Public API: none. Wire: none.
