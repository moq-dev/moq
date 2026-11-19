# [S] A JS lite-07 stale drop narrows its budget every run

## Goal

`js/net`'s `a stale drop is recorded under the narrowest budget asked on lite-07`
(`src/lite/subscriber.test.ts`) passes every run.

## Plan

Seen 2026-11-18 while validating
[moq-dev/moq#9128](https://github.com/moq-dev/moq/pull/9128), on a base that
touched no JS: run alone with `bun test src/lite/subscriber.test.ts -t 'stale drop
is recorded under the narrowest'` from `js/net`, it failed 4 of 5 runs. The second
narrowing (`track.update({ delay: { max: 200ms } })`) leaves `entry.staleBudget` at
500 after `drainReactions()`. The update reaches the subscriber (`subscribe update`
is logged), so either the narrowing is applied after the drain returns, or the
entry it narrows is not the one the test holds. Find which and fix that, not the
drain.

Public API: none. Wire: none.
