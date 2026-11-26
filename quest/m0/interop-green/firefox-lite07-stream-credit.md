# [M] Firefox's lite-07 stream-credit subscriptions close when the publisher finishes

## Goal

The scheduled Interop step `Stream credit in browsers` (`just test stream-credit`) passes its
`firefox on moq-lite-07-wip` cell: every one of the tab's 300 subscriptions ends within 10 s of
the publisher finishing.

## Plan

Found by the Nightly triage of 2026-11-26, in scheduled Interop run
[45873739689](https://github.com/moq-dev/moq/actions/runs/45873739689) at `b50932876b`:

```
FAIL 144 subscriptions still open 10000 ms after the publisher finished
FAIL mid-1 is still open; last groups 35:2:TooFarBehind: lagged: frames were evicted before being read 37:2:Stream: remote error: 52 38:2:Stream: remote error: 52 ...
```

Chromium on lite-07 and both browsers on lite-06 passed. The step passed in the scheduled run
at `420b8dcb0d` ([45675453343](https://github.com/moq-dev/moq/actions/runs/45675453343)), so
a change in `420b8dcb0d..b50932876b` likely caused it. The lite-07 group accounting changes are
the first suspects, notably [moq-dev/moq#9686](https://github.com/moq-dev/moq/pull/9686)
(settle starts via GROUP_END and SUBSCRIBE_DROP(SKIP)), then
[moq-dev/moq#9644](https://github.com/moq-dev/moq/pull/9644) and
[moq-dev/moq#9564](https://github.com/moq-dev/moq/pull/9564). The tracks still open had
groups reset with code 52 after falling too far behind, so check that a subscription whose
last groups were reset still learns the track ended.

Reproduce with the Firefox lite-07 cell alone, measure the rate, then bisect.

Public API: none expected. Wire: none expected.
