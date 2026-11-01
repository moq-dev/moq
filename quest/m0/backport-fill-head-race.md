# [M] Backport the joined-head FETCH race fix to release

## Goal

A relay on `release` no longer FETCHes the head of a group that its own
SUBSCRIBE is already bringing. In that race the fetched objects can land first,
so the relay caches the group as fetched, refuses the live tail stream that ends
it as `duplicate`, and leaves live readers waiting on the group until they time
out. This ports fix 2 of
[moq-dev/moq#7135](https://github.com/moq-dev/moq/pull/7135) and ships it in a
`moq-net` patch release.

## Plan

Decided 2026-10-30 (maintainer): port only the relay fix. Why: #7135's other
fix, the JS publisher answering an `INCLUDE_PROPERTIES=0` FETCH without
timestamps, cannot happen on `release`, whose `js/net/src/ietf/publisher.ts`
`runFetch` refuses every FETCH.

Decided 2026-10-30 (maintainer): m0, beside the other `release` backports.
Why: it is an intermittent relay hang on the shipped line.

`release` has the same machinery as main before #7135: the joining-head
`Fill` in `rs/moq-net/src/ietf/subscriber.rs` and the group FETCH that fills a
cache miss (`run_group_fetch`). It has no `rs/moq-net/src/model/track/fill.rs`,
so the fix is hand-ported, not cherry-picked. Port the behavior: a cache miss
for the joined group waits until the subscription's own head arrives or its
request settles, then the cache answers it; a failed head request still falls
back to a FETCH. Only that group waits, so older history still FETCHes at once,
and the wait ends when no reader wants the group or the track aborts. Port
#7135's subscriber tests that cover this alongside it.

Start by reproducing the race on `release` with one of those tests. If
`release` cannot hit it, delete this quest and say why in the PR.

Public API: none. Wire: none.

## Required

- [The browser backend baseline completes on IETF22](/quest/m0/nightly/browser-baseline.md) - lands the fix this quest ports, on main
