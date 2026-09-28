# [L] Sans-IO model

## Goal

The origin, broadcast, track, group, and frame producers and consumers run
without an async runtime. Their waiting is poll-based on kio, and anything
time-based reads a clock the caller supplies, so the model translates to
TypeScript and its tests can run on a mock clock.

## Plan

The model is already poll-based on kio waiters; what remains is every place
that reaches a runtime or wall clock directly (`runtime::Deadline`, the cache
pool's expiry, stats timers). Route time through one injectable clock, the
seam the [mock-clock tests](/quest/m1/rs2ts/mock-clock.md) use.

Public API: may break moq-net's model constructors; retargets to `dev`.
Wire: none.
