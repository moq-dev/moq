# [XS] The JS stalled-consumer eviction test ages its groups

## Goal

`js/net/src/broadcast.test.ts` "a stalled consumer does not pin evicted
groups" ages its groups past `maxAge` on the clock the cache actually reads,
so it fails if a stalled consumer does pin evicted groups.

## Plan

Decided 2026-10-29 (maintainer), a follow-up of
[moq-dev/moq#7076](https://github.com/moq-dev/moq/pull/7076). The test calls
`setSystemTime`, but the cache ages groups on the monotonic clock, so that
step does nothing. Drive a `Manual` clock past `maxAge` instead, as the
other converted tests do, and restore the ambient clock when it finishes.
Prove the test fails when the consumer is made to pin evicted groups.

Public API: none. Wire: none.
