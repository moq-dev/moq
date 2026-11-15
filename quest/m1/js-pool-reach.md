# [S] The JavaScript byte pool carries only what a JS caller can reach

## Goal

`js/net`'s per-broadcast byte pool keeps idle expiry, drop-run expiry, and the
per-group frame tail cap, and carries byte eviction only if a JS consumer sets
a capacity. Code that no production path reaches leaves the bundle.

## Plan

Found 2026-11-14 by the [feature review](/quest/m3/release/feature-review.md).
The pool's capacity defaults to infinity (`js/net/src/cache.ts`), every pool
is built without one (`broadcast.ts`, `track.ts`), and `resize` has no
non-test caller, so `Pool.accrue` always returns `undefined` and `#payDebt`,
reader debt, group `#shed`, and the debt calls in `track.ts` run only in
tests. [moq-dev/moq#8177](https://github.com/moq-dev/moq/pull/8177) said so
("byte eviction runs only for a pool given one. Today only tests do that"),
yet [moq-dev/moq#8294](https://github.com/moq-dev/moq/pull/8294),
[moq-dev/moq#8440](https://github.com/moq-dev/moq/pull/8440), and
[moq-dev/moq#8673](https://github.com/moq-dev/moq/pull/8673) kept building
on it. Rust differs: the relay sets a capacity. Expiry alone already bounds
the drop set (#8294).

This revisits the pool quest's decision to port it "with no new setting"
(deleted by #8177), with the evidence that two weeks on nothing reaches the
path.

Delete byte debt, shed, reader debt, and run billing from JS,
keeping the expiries; restore them from git with a consumer.

Decided 2026-11-14 (maintainer, pick on [moq-dev/moq#8764](https://github.com/moq-dev/moq/pull/8764)): ✅ (a) delete the unreachable eviction (rejected: (b) expose `capacity`; (c) keep it for parity).

Public API: none. Wire: none.
